use crate::analysis::{BtcState, CoinMetrics};
use crate::feeds::types::*;
use crate::feeds::FeedMessage;
use eframe::egui;
use std::collections::HashMap;
use tokio::sync::mpsc;

/// App state shared between the async backend and the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppPhase {
    /// Initial loading — fetching coin list.
    Initializing,
    /// Backfilling historical data.
    Backfilling,
    /// Running normally.
    Live,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketFilter {
    All,
    Perps,
    Spot,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Coin,
    Price,
    Volume,
    Volatility(Interval),
    RelativeStrength,
    Beta,
    Score,
}

pub struct CryptoScreenerApp {
    // Data
    pub metrics: HashMap<String, CoinMetrics>,
    pub btc_state: BtcState,

    // Backfill state
    pub phase: AppPhase,
    pub backfill_coin: String,
    pub backfill_current: usize,
    pub backfill_total: usize,

    // Connection
    pub ws_connected: bool,
    pub last_update: std::time::Instant,
    pub coins_tracked: usize,
    pub db_size: u64,

    // UI state
    pub sort_column: SortColumn,
    pub sort_ascending: bool,
    pub market_filter: MarketFilter,
    pub search_query: String,
    pub min_volume: f64,
    pub min_volatility: f64,

    // Channel to receive messages from the async backend
    pub rx: mpsc::UnboundedReceiver<FeedMessage>,
}

impl CryptoScreenerApp {
    pub fn new(
        _cc: &eframe::CreationContext<'_>,
        rx: mpsc::UnboundedReceiver<FeedMessage>,
    ) -> Self {
        Self {
            metrics: HashMap::new(),
            btc_state: BtcState::default(),
            phase: AppPhase::Initializing,
            backfill_coin: String::new(),
            backfill_current: 0,
            backfill_total: 0,
            ws_connected: false,
            last_update: std::time::Instant::now(),
            coins_tracked: 0,
            db_size: 0,
            sort_column: SortColumn::Score,
            sort_ascending: false,
            market_filter: MarketFilter::All,
            search_query: String::new(),
            min_volume: 0.0,
            min_volatility: 0.0,
            rx,
        }
    }

    /// Drain all pending messages from the async backend.
    fn process_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                FeedMessage::CoinList(coins) => {
                    self.coins_tracked = coins.len();
                    self.phase = AppPhase::Backfilling;
                    for coin in &coins {
                        self.metrics
                            .entry(coin.name.clone())
                            .or_insert_with(|| CoinMetrics {
                                coin: coin.name.clone(),
                                is_perp: coin.is_perp,
                                price: coin.price,
                                volume_24h: coin.volume_24h,
                                ..Default::default()
                            });
                    }
                }
                FeedMessage::Progress(p) => {
                    self.backfill_coin = p.coin;
                    self.backfill_current = p.current;
                    self.backfill_total = p.total;
                    if p.current == p.total {
                        self.phase = AppPhase::Live;
                    }
                }
                FeedMessage::CandleBatch(_candles) => {
                    // Candles are stored in SQLite by the backend.
                    // We'll trigger a metrics recalc periodically.
                    self.last_update = std::time::Instant::now();
                }
                FeedMessage::LiveCandle(candle) => {
                    if let Some(m) = self.metrics.get_mut(&candle.coin) {
                        m.price = candle.close;
                    }
                    self.last_update = std::time::Instant::now();
                }
                FeedMessage::Connected(status) => {
                    self.ws_connected = status;
                }
                FeedMessage::Error(e) => {
                    tracing::error!("Feed error: {e}");
                }
            }
        }
    }

    /// Get sorted and filtered coin list for display.
    fn sorted_coins(&self) -> Vec<&CoinMetrics> {
        let mut coins: Vec<&CoinMetrics> = self
            .metrics
            .values()
            .filter(|m| match self.market_filter {
                MarketFilter::All => true,
                MarketFilter::Perps => m.is_perp,
                MarketFilter::Spot => !m.is_perp,
            })
            .filter(|m| {
                self.search_query.is_empty()
                    || m.coin
                        .to_lowercase()
                        .contains(&self.search_query.to_lowercase())
            })
            .filter(|m| m.volume_24h >= self.min_volume)
            .collect();

        coins.sort_by(|a, b| {
            let ord = match self.sort_column {
                SortColumn::Coin => a.coin.cmp(&b.coin),
                SortColumn::Price => a.price.partial_cmp(&b.price).unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::Volume => a.volume_24h.partial_cmp(&b.volume_24h).unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::Volatility(interval) => {
                    let va = a.volatility.get(&interval).unwrap_or(&0.0);
                    let vb = b.volatility.get(&interval).unwrap_or(&0.0);
                    va.partial_cmp(vb).unwrap_or(std::cmp::Ordering::Equal)
                }
                SortColumn::RelativeStrength => a.relative_strength.partial_cmp(&b.relative_strength).unwrap_or(std::cmp::Ordering::Equal),
                SortColumn::Beta => {
                    let ba = avg_map(&a.btc_beta);
                    let bb = avg_map(&b.btc_beta);
                    ba.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal)
                }
                SortColumn::Score => a.score.partial_cmp(&b.score).unwrap_or(std::cmp::Ordering::Equal),
            };
            if self.sort_ascending { ord } else { ord.reverse() }
        });

        coins
    }

    /// Get coins currently firing signals.
    fn signal_coins(&self) -> Vec<&CoinMetrics> {
        if !self.btc_state.is_dipping {
            return Vec::new();
        }
        let mut signals: Vec<&CoinMetrics> = self
            .metrics
            .values()
            .filter(|m| m.score >= 70.0 && m.relative_strength > 0.0 && m.volume_24h >= 100_000.0)
            .collect();
        signals.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        signals
    }
}

impl eframe::App for CryptoScreenerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_messages();

        // Request repaint every 500ms for live updates
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        match self.phase {
            AppPhase::Initializing => {
                render_loading(ctx, "Connecting to Hyperliquid...", 0, 0, "");
            }
            AppPhase::Backfilling => {
                render_loading(
                    ctx,
                    "Backfilling historical data",
                    self.backfill_current,
                    self.backfill_total,
                    &self.backfill_coin,
                );
            }
            AppPhase::Live => {
                self.render_live(ctx);
            }
        }
    }
}

impl CryptoScreenerApp {
    fn render_live(&mut self, ctx: &egui::Context) {
        // Top panel — BTC status
        egui::TopBottomPanel::top("btc_status").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("BTC:");
                ui.monospace(format!("${:.2}", self.btc_state.price));
                ui.separator();
                let change_color = if self.btc_state.change_24h_pct >= 0.0 {
                    egui::Color32::from_rgb(0, 200, 80)
                } else {
                    egui::Color32::from_rgb(255, 80, 80)
                };
                ui.colored_label(change_color, format!("24h: {:.1}%", self.btc_state.change_24h_pct));
                ui.separator();

                let dd_color = if self.btc_state.drawdown_pct < -2.0 {
                    egui::Color32::from_rgb(255, 80, 80)
                } else {
                    egui::Color32::from_rgb(180, 180, 180)
                };
                ui.colored_label(dd_color, format!("Drawdown: {:.1}%", self.btc_state.drawdown_pct));
                ui.separator();

                if self.btc_state.is_dipping {
                    ui.colored_label(
                        egui::Color32::from_rgb(255, 160, 0),
                        format!(
                            "DIPPING ({})",
                            format_duration(self.btc_state.dip_duration_secs)
                        ),
                    );
                } else {
                    ui.label("Normal");
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let conn_color = if self.ws_connected {
                        egui::Color32::from_rgb(0, 200, 80)
                    } else {
                        egui::Color32::from_rgb(255, 80, 80)
                    };
                    ui.colored_label(conn_color, if self.ws_connected { "● Connected" } else { "● Disconnected" });
                });
            });
        });

        // Bottom panel — status bar
        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                let elapsed = self.last_update.elapsed().as_secs_f64();
                ui.label(format!("Last update: {:.1}s ago", elapsed));
                ui.separator();
                ui.label(format!("Coins: {}", self.coins_tracked));
                ui.separator();
                ui.label(format!("DB: {}", format_bytes(self.db_size)));
            });
        });

        // Central panel
        egui::CentralPanel::default().show(ctx, |ui| {
            // Filter bar
            ui.horizontal(|ui| {
                ui.label("Search:");
                ui.text_edit_singleline(&mut self.search_query);
                ui.separator();

                ui.label("Min Vol 24h:");
                ui.add(egui::DragValue::new(&mut self.min_volume).speed(10000.0).prefix("$"));
                ui.separator();

                ui.label("Type:");
                ui.selectable_value(&mut self.market_filter, MarketFilter::All, "All");
                ui.selectable_value(&mut self.market_filter, MarketFilter::Perps, "Perps");
                ui.selectable_value(&mut self.market_filter, MarketFilter::Spot, "Spot");
            });

            ui.add_space(8.0);

            // Signals section
            let signals = self.signal_coins();
            if !signals.is_empty() {
                ui.heading(
                    egui::RichText::new(format!("SIGNALS ({})", signals.len()))
                        .color(egui::Color32::from_rgb(255, 200, 0)),
                );
                render_coin_table(ui, &signals, "signals_table");
                ui.add_space(12.0);
            } else if self.btc_state.is_dipping {
                ui.colored_label(
                    egui::Color32::from_rgb(180, 180, 0),
                    "BTC dipping — no coins meeting signal criteria yet",
                );
                ui.add_space(8.0);
            }

            // All coins table
            ui.heading("All Coins");

            let coins = self.sorted_coins();
            render_coin_table(ui, &coins, "all_coins_table");
        });
    }
}

fn render_coin_table(ui: &mut egui::Ui, coins: &[&CoinMetrics], id: &str) {
    egui::ScrollArea::both()
        .id_salt(id)
        .show(ui, |ui| {
            egui::Grid::new(id)
                .striped(true)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    // Header
                    ui.strong("Coin");
                    ui.strong("Price");
                    ui.strong("Vol 24h");
                    for interval in Interval::all() {
                        ui.strong(format!("V.{}", interval.as_str()));
                    }
                    ui.strong("RS%");
                    ui.strong("Beta");
                    ui.strong("Score");
                    ui.end_row();

                    // Rows
                    for m in coins {
                        ui.label(&m.coin);
                        ui.monospace(format_price(m.price));
                        ui.monospace(format_volume(m.volume_24h));

                        for interval in Interval::all() {
                            let vol = m.volatility.get(interval).unwrap_or(&0.0);
                            let color = volatility_color(*vol);
                            ui.colored_label(color, format!("{:.2}", vol));
                        }

                        let rs_color = if m.relative_strength >= 0.0 {
                            egui::Color32::from_rgb(0, 200, 80)
                        } else {
                            egui::Color32::from_rgb(255, 80, 80)
                        };
                        ui.colored_label(rs_color, format!("{:+.1}%", m.relative_strength * 100.0));

                        let avg_beta = avg_map(&m.btc_beta);
                        ui.monospace(format!("{:.1}", avg_beta));

                        let score_color = score_color(m.score);
                        ui.colored_label(score_color, format!("{:.0}", m.score));
                        ui.end_row();
                    }
                });
        });
}

fn render_loading(ctx: &egui::Context, message: &str, current: usize, total: usize, coin: &str) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);

            ui.heading("CryptoScreener");
            ui.add_space(20.0);

            ui.label(message);
            ui.add_space(10.0);

            if total > 0 {
                let progress = current as f32 / total as f32;
                ui.add(
                    egui::ProgressBar::new(progress)
                        .text(format!("{current}/{total}")),
                );
                ui.add_space(8.0);
                ui.label(format!("Loading: {coin}"));
                ui.add_space(8.0);

                let remaining = total - current;
                ui.small(format!(
                    "~{} remaining",
                    format_duration((remaining as i64) * 2) // rough estimate: ~2s per coin
                ));
            } else {
                ui.spinner();
            }

            ui.add_space(20.0);
            ui.small("First launch requires downloading historical data.");
            ui.small("Subsequent launches will be near-instant.");
        });
    });
}

// --- Formatting helpers ---

fn format_price(price: f64) -> String {
    if price >= 1000.0 {
        format!("${:.2}", price)
    } else if price >= 1.0 {
        format!("${:.4}", price)
    } else if price >= 0.01 {
        format!("${:.6}", price)
    } else {
        format!("${:.8}", price)
    }
}

fn format_volume(vol: f64) -> String {
    if vol >= 1_000_000_000.0 {
        format!("${:.1}B", vol / 1_000_000_000.0)
    } else if vol >= 1_000_000.0 {
        format!("${:.1}M", vol / 1_000_000.0)
    } else if vol >= 1_000.0 {
        format!("${:.1}K", vol / 1_000.0)
    } else {
        format!("${:.0}", vol)
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes >= 1_073_741_824 {
        format!("{:.1}GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_048_576 {
        format!("{:.1}MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1_024 {
        format!("{:.1}KB", bytes as f64 / 1_024.0)
    } else {
        format!("{}B", bytes)
    }
}

fn format_duration(secs: i64) -> String {
    if secs >= 86400 {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
    } else if secs >= 3600 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else if secs >= 60 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}s", secs)
    }
}

fn volatility_color(vol: f64) -> egui::Color32 {
    if vol >= 0.05 {
        egui::Color32::from_rgb(255, 80, 80) // hot
    } else if vol >= 0.03 {
        egui::Color32::from_rgb(255, 180, 0) // warm
    } else if vol >= 0.01 {
        egui::Color32::from_rgb(200, 200, 100) // mild
    } else {
        egui::Color32::from_rgb(150, 150, 150) // cool
    }
}

fn score_color(score: f64) -> egui::Color32 {
    if score >= 80.0 {
        egui::Color32::from_rgb(0, 255, 100)
    } else if score >= 60.0 {
        egui::Color32::from_rgb(180, 220, 0)
    } else if score >= 40.0 {
        egui::Color32::from_rgb(200, 200, 100)
    } else {
        egui::Color32::from_rgb(150, 150, 150)
    }
}

fn avg_map(map: &HashMap<Interval, f64>) -> f64 {
    if map.is_empty() {
        0.0
    } else {
        map.values().sum::<f64>() / map.len() as f64
    }
}
