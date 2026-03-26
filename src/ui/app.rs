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
    Change24h,
    Volume,
    OpenInterest,
    FundingRate,
    Volatility(Interval),
    VsBtc(Interval),
    Beta,
}

pub struct VsBtcApp {
    // Data
    pub metrics: HashMap<String, CoinMetrics>,
    pub btc_state: BtcState,

    // Backfill state
    pub phase: AppPhase,
    pub backfill_coin: String,
    pub backfill_current: usize,
    pub backfill_total: usize,
    pub backfill_fetched: usize,
    pub backfill_skipped: usize,
    pub backfill_phase: &'static str,

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

impl VsBtcApp {
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
            backfill_fetched: 0,
            backfill_skipped: 0,
            backfill_phase: "",
            ws_connected: false,
            last_update: std::time::Instant::now(),
            coins_tracked: 0,
            db_size: 0,
            sort_column: SortColumn::OpenInterest,
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
                    if self.phase == AppPhase::Initializing {
                        self.phase = AppPhase::Backfilling;
                    }
                    for coin in &coins {
                        let entry = self.metrics
                            .entry(coin.name.clone())
                            .or_insert_with(|| CoinMetrics {
                                coin: coin.name.clone(),
                                is_perp: coin.is_perp,
                                ..Default::default()
                            });
                        // Always refresh market data from coin list
                        entry.change_24h_pct = coin.change_24h_pct;
                        entry.volume_24h = coin.volume_24h;
                        entry.open_interest = coin.open_interest;
                        entry.funding_rate = coin.funding_rate;
                        // Only update price if we don't have a live price yet
                        if entry.price == 0.0 {
                            entry.price = coin.price;
                        }
                    }
                }
                FeedMessage::Progress(p) => {
                    self.backfill_coin = p.coin;
                    self.backfill_current = p.current;
                    self.backfill_total = p.total;
                    self.backfill_fetched = p.fetched;
                    self.backfill_skipped = p.skipped;
                    self.backfill_phase = p.phase;
                    if p.current == p.total && p.phase == "Background" {
                        self.phase = AppPhase::Live;
                    }
                }
                FeedMessage::CandleBatch(_candles) => {
                    // Candles are stored in SQLite by the backend.
                    // We'll trigger a metrics recalc periodically.
                    self.last_update = std::time::Instant::now();
                }
                FeedMessage::LiveCandle(_candle) => {
                    self.last_update = std::time::Instant::now();
                }
                FeedMessage::PriceUpdate(prices) => {
                    for (coin, price) in &prices {
                        if let Some(m) = self.metrics.get_mut(coin) {
                            m.price = *price;
                        }
                    }
                    // Update BTC price in header immediately
                    if let Some(btc_price) = prices.get("BTC") {
                        self.btc_state.price = *btc_price;
                    }
                    self.last_update = std::time::Instant::now();
                }
                FeedMessage::MetricsUpdate(all_metrics) => {
                    for m in all_metrics {
                        self.metrics.insert(m.coin.clone(), m);
                    }
                    self.last_update = std::time::Instant::now();
                }
                FeedMessage::BtcStateUpdate(state) => {
                    self.btc_state = state;
                }
                FeedMessage::DbSize(size) => {
                    self.db_size = size;
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
            .filter(|m| {
                self.search_query.is_empty()
                    || m.coin
                        .to_lowercase()
                        .contains(&self.search_query.to_lowercase())
            })
            .filter(|m| m.volume_24h >= self.min_volume)
            .filter(|m| {
                self.min_volatility <= 0.0
                    || avg_map(&m.volatility) >= self.min_volatility
            })
            .collect();

        coins.sort_by(|a, b| {
            let cmp = |x: f64, y: f64| x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal);
            let ord = match self.sort_column {
                SortColumn::Coin => a.coin.cmp(&b.coin),
                SortColumn::Price => cmp(a.price, b.price),
                SortColumn::Change24h => cmp(a.change_24h_pct, b.change_24h_pct),
                SortColumn::Volume => cmp(a.volume_24h, b.volume_24h),
                SortColumn::OpenInterest => cmp(a.open_interest, b.open_interest),
                SortColumn::FundingRate => cmp(a.funding_rate, b.funding_rate),
                SortColumn::Volatility(interval) => {
                    let va = a.volatility.get(&interval).unwrap_or(&0.0);
                    let vb = b.volatility.get(&interval).unwrap_or(&0.0);
                    cmp(*va, *vb)
                }
                SortColumn::VsBtc(interval) => {
                    let va = a.vs_btc.get(&interval).unwrap_or(&0.0);
                    let vb = b.vs_btc.get(&interval).unwrap_or(&0.0);
                    cmp(*va, *vb)
                }
                SortColumn::Beta => cmp(avg_map(&a.btc_beta), avg_map(&b.btc_beta)),
            };
            if self.sort_ascending { ord } else { ord.reverse() }
        });

        coins
    }

}

impl eframe::App for VsBtcApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.process_messages();

        // Request repaint every 500ms for live updates
        ctx.request_repaint_after(std::time::Duration::from_millis(500));

        match self.phase {
            AppPhase::Initializing => {
                render_loading(ctx, "Connecting to Hyperliquid...", 0, 0, "");
            }
            AppPhase::Backfilling | AppPhase::Live => {
                self.render_live(ctx);
            }
        }
    }
}

impl VsBtcApp {
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
            if self.phase == AppPhase::Backfilling && self.backfill_total > 0 {
                let progress = self.backfill_current as f32 / self.backfill_total as f32;
                let remaining = self.backfill_total - self.backfill_current;
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("[{}]", self.backfill_phase))
                            .color(egui::Color32::from_rgb(255, 200, 0)),
                    );
                    ui.add(
                        egui::ProgressBar::new(progress)
                            .text(format!(
                                "{}/{} — {} (fetched: {}, skipped: {})",
                                self.backfill_current,
                                self.backfill_total,
                                self.backfill_coin,
                                self.backfill_fetched,
                                self.backfill_skipped,
                            ))
                            .desired_width(ui.available_width() - 10.0),
                    );
                });
            } else {
                ui.horizontal(|ui| {
                    let elapsed = self.last_update.elapsed().as_secs_f64();
                    ui.label(format!("Last update: {:.1}s ago", elapsed));
                    ui.separator();
                    ui.label(format!("Coins: {}", self.coins_tracked));
                    ui.separator();
                    ui.label(format!("DB: {}", format_bytes(self.db_size)));
                });
            }
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

            });

            ui.add_space(8.0);

            let coins = self.sorted_coins();
            let sort_col = self.sort_column;
            let sort_asc = self.sort_ascending;
            let (new_col, new_asc) = render_coin_table(ui, &coins, "all_coins_table", sort_col, sort_asc);
            self.sort_column = new_col;
            self.sort_ascending = new_asc;
        });
    }
}

fn sort_header(
    ui: &mut egui::Ui,
    label: &str,
    column: SortColumn,
    current_col: SortColumn,
    current_asc: bool,
) -> Option<(SortColumn, bool)> {
    let arrow = if current_col == column {
        if current_asc { " ▲" } else { " ▼" }
    } else {
        ""
    };
    let text = format!("{label}{arrow}");
    let response = ui.add(
        egui::Label::new(egui::RichText::new(text).strong())
            .sense(egui::Sense::click()),
    );
    if response.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        if current_col == column {
            Some((column, !current_asc))
        } else {
            Some((column, false))
        }
    } else {
        None
    }
}

fn render_coin_table(
    ui: &mut egui::Ui,
    coins: &[&CoinMetrics],
    id: &str,
    sort_col: SortColumn,
    sort_asc: bool,
) -> (SortColumn, bool) {
    let mut new_sort = (sort_col, sort_asc);

    egui::ScrollArea::both()
        .id_salt(id)
        .show(ui, |ui| {
            egui::Grid::new(id)
                .striped(true)
                .spacing([8.0, 4.0])
                .show(ui, |ui| {
                    // Headers — CEX-style order then vsBTC columns
                    let mut headers: Vec<(String, SortColumn)> = vec![
                        ("Coin".into(), SortColumn::Coin),
                        ("Price".into(), SortColumn::Price),
                        ("24h%".into(), SortColumn::Change24h),
                        ("Vol 24h".into(), SortColumn::Volume),
                        ("OI".into(), SortColumn::OpenInterest),
                        ("Funding".into(), SortColumn::FundingRate),
                    ];
                    for interval in Interval::all() {
                        headers.push((
                            format!("V.{}", interval.as_str()),
                            SortColumn::Volatility(*interval),
                        ));
                    }
                    for interval in Interval::all() {
                        headers.push((
                            format!("vs.{}", interval.as_str()),
                            SortColumn::VsBtc(*interval),
                        ));
                    }
                    headers.push(("Beta".into(), SortColumn::Beta));

                    for (label, col) in &headers {
                        if let Some(s) = sort_header(ui, label, *col, sort_col, sort_asc) {
                            new_sort = s;
                        }
                    }
                    ui.end_row();

                    // Rows
                    for m in coins {
                        let coin_link = ui.add(
                            egui::Label::new(
                                egui::RichText::new(&m.coin).color(egui::Color32::from_rgb(100, 180, 255))
                            ).sense(egui::Sense::click()),
                        );
                        if coin_link.clicked() {
                            let url = format!("https://app.hyperliquid.xyz/trade/{}", m.coin);
                            let _ = open::that(&url);
                        }
                        if coin_link.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        ui.monospace(format_price(m.price));

                        // 24h change — green/red
                        let change_color = if m.change_24h_pct >= 0.0 {
                            egui::Color32::from_rgb(0, 200, 80)
                        } else {
                            egui::Color32::from_rgb(255, 80, 80)
                        };
                        ui.colored_label(change_color, format!("{:+.2}%", m.change_24h_pct));

                        ui.monospace(format_volume(m.volume_24h));

                        ui.monospace(format_volume(m.open_interest));

                        let fr_color = if m.funding_rate >= 0.0 {
                            egui::Color32::from_rgb(0, 200, 80)
                        } else {
                            egui::Color32::from_rgb(255, 80, 80)
                        };
                        ui.colored_label(fr_color, format!("{:+.4}%", m.funding_rate * 100.0));

                        // Volatility per timeframe
                        let no_data = egui::Color32::from_rgb(80, 80, 80);
                        for interval in Interval::all() {
                            match m.volatility.get(interval) {
                                Some(vol) => {
                                    let color = volatility_color(*vol);
                                    ui.colored_label(color, format!("{:.2}", vol));
                                }
                                None => { ui.colored_label(no_data, "—"); }
                            }
                        }

                        // vs BTC per timeframe
                        for interval in Interval::all() {
                            match m.vs_btc.get(interval) {
                                Some(vs) => {
                                    let pct = vs * 100.0;
                                    let color = if pct >= 0.0 {
                                        egui::Color32::from_rgb(0, 200, 80)
                                    } else {
                                        egui::Color32::from_rgb(255, 80, 80)
                                    };
                                    ui.colored_label(color, format!("{:+.1}%", pct));
                                }
                                None => { ui.colored_label(no_data, "—"); }
                            }
                        }

                        if m.btc_beta.is_empty() {
                            ui.colored_label(no_data, "—");
                        } else {
                            ui.monospace(format!("{:.1}", avg_map(&m.btc_beta)));
                        }
                        ui.end_row();
                    }
                });
        });

    new_sort
}

fn render_loading(ctx: &egui::Context, message: &str, current: usize, total: usize, coin: &str) {
    egui::CentralPanel::default().show(ctx, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);

            ui.heading("vsBTC");
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
