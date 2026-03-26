mod analysis;
mod feeds;
mod store;
mod ui;

use analysis::{AnalysisEngine, CoinMetrics};
use chrono::{DateTime, Utc};
use feeds::hyperliquid::HyperliquidFeed;
use feeds::FeedMessage;
use store::Database;
use tokio::sync::{mpsc, oneshot};
use tracing::info;

fn load_icon() -> egui::IconData {
    let png_bytes = include_bytes!("../assets/icon.png");
    let image = image::load_from_memory(png_bytes).expect("Failed to load icon");
    let rgba = image.to_rgba8();
    let (w, h) = rgba.dimensions();
    egui::IconData {
        rgba: rgba.into_raw(),
        width: w,
        height: h,
    }
}

fn main() -> eframe::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");

    let (tx, rx) = mpsc::unbounded_channel::<FeedMessage>();

    rt.spawn(async move {
        run_data_pipeline(tx).await;
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([900.0, 500.0])
            .with_title("vsBTC")
            .with_icon(std::sync::Arc::new(load_icon()))
            .with_app_id("vsbtc"),
        ..Default::default()
    };

    eframe::run_native(
        "vsbtc",
        options,
        Box::new(|cc| Ok(Box::new(ui::VsBtcApp::new(cc, rx)))),
    )
}

/// Commands sent to the database thread.
enum DbCommand {
    UpsertCoins(Vec<feeds::CoinInfo>),
    UpsertCandles(Vec<feeds::Candle>),
    /// Query the latest candle time for a coin/interval.
    LatestCandleTime {
        coin: String,
        interval: feeds::Interval,
        reply: oneshot::Sender<Option<DateTime<Utc>>>,
    },
    ComputeMetrics {
        coins: Vec<feeds::CoinInfo>,
        tx: mpsc::UnboundedSender<FeedMessage>,
    },
    Prune,
}

/// Spawn the database thread. Returns a sender for commands.
fn spawn_db_thread() -> mpsc::UnboundedSender<DbCommand> {
    let (db_tx, mut db_rx) = mpsc::unbounded_channel::<DbCommand>();

    std::thread::spawn(move || {
        let db = match Database::open() {
            Ok(db) => db,
            Err(e) => {
                tracing::error!("Database error: {e}");
                return;
            }
        };

        while let Some(cmd) = db_rx.blocking_recv() {
            match cmd {
                DbCommand::UpsertCoins(coins) => {
                    if let Err(e) = db.upsert_coins(&coins) {
                        tracing::error!("Failed to store coins: {e}");
                    }
                }
                DbCommand::UpsertCandles(candles) => {
                    if let Err(e) = db.upsert_candles(&candles) {
                        tracing::error!("Failed to store candles: {e}");
                    }
                }
                DbCommand::LatestCandleTime {
                    coin,
                    interval,
                    reply,
                } => {
                    let result = db.latest_candle_time(&coin, interval).unwrap_or(None);
                    let _ = reply.send(result);
                }
                DbCommand::ComputeMetrics { coins, tx } => {
                    compute_and_send_metrics(&db, &coins, &tx);
                }
                DbCommand::Prune => {
                    if let Err(e) = db.prune() {
                        tracing::error!("Prune failed: {e}");
                    }
                }
            }
        }
    });

    db_tx
}

async fn run_data_pipeline(tx: mpsc::UnboundedSender<FeedMessage>) {
    let db_tx = spawn_db_thread();
    let feed = HyperliquidFeed::new();

    // Fetch coin list
    info!("Fetching coin list...");
    let coins = match feed.fetch_coin_list().await {
        Ok(coins) if coins.is_empty() => {
            let _ = tx.send(FeedMessage::Error(
                "No coins found on Hyperliquid".to_string(),
            ));
            return;
        }
        Ok(coins) => {
            let _ = db_tx.send(DbCommand::UpsertCoins(coins.clone()));
            let _ = tx.send(FeedMessage::CoinList(coins.clone()));
            coins
        }
        Err(e) => {
            let _ = tx.send(FeedMessage::Error(format!("Failed to fetch coins: {e}")));
            return;
        }
    };

    // Sort coins: BTC first, then by volume descending
    let mut sorted_coins = coins.clone();
    sorted_coins.sort_by(|a, b| {
        if a.name == "BTC" {
            return std::cmp::Ordering::Less;
        }
        if b.name == "BTC" {
            return std::cmp::Ordering::Greater;
        }
        b.volume_24h
            .partial_cmp(&a.volume_24h)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Rate limit: 1200 weight/min, candleSnapshot = ~23 weight each
    // Safe target: ~45 requests/min = ~1.3s between requests
    // Phase 2 (priority): no throttle, burst through the small batch
    // Phase 3 (background): 1.4s delay between requests

    let priority_intervals = [
        feeds::Interval::H1,
        feeds::Interval::H4,
        feeds::Interval::D1,
    ];
    let priority_count = 21; // BTC + top 20

    let total = sorted_coins.len();
    let mut fetched_total = 0usize;
    let mut skipped_total = 0usize;

    // --- Phase 2: BTC + top 20, key intervals only ---
    info!("Phase 2: loading BTC + top 20 coins (1h, 4h, 1d)...");
    let phase2_coins = &sorted_coins[..priority_count.min(total)];

    for (i, coin) in phase2_coins.iter().enumerate() {
        for interval in &priority_intervals {
            let did_fetch = fetch_candle_gap(&feed, &db_tx, &tx, coin, *interval).await;
            if did_fetch { fetched_total += 1; } else { skipped_total += 1; }
        }
        let _ = tx.send(FeedMessage::Progress(feeds::BackfillProgress {
            coin: coin.name.clone(),
            current: i + 1,
            total,
            fetched: fetched_total,
            skipped: skipped_total,
            phase: "Priority",
        }));
    }

    // Compute metrics now — user has usable data
    let _ = db_tx.send(DbCommand::ComputeMetrics {
        coins: coins.clone(),
        tx: tx.clone(),
    });
    info!(
        "Phase 2 complete — fetched: {}, skipped: {}",
        fetched_total, skipped_total
    );

    // Start WebSocket immediately after phase 2 so we get live data during background backfill
    info!("Starting live feed...");
    let (live_tx, mut live_rx) = mpsc::unbounded_channel::<FeedMessage>();
    let forward_tx = tx.clone();
    let forward_db_tx = db_tx.clone();
    tokio::spawn(async move {
        while let Some(msg) = live_rx.recv().await {
            if let FeedMessage::LiveCandle(ref candle) = msg {
                let _ = forward_db_tx.send(DbCommand::UpsertCandles(vec![candle.clone()]));
            }
            let _ = forward_tx.send(msg);
        }
    });
    let ws_coins = coins.clone();
    tokio::spawn(async move {
        HyperliquidFeed::stream_live(&ws_coins, live_tx).await;
    });

    // Spawn periodic metrics recomputation (every 10 seconds)
    let metrics_tx = tx.clone();
    let metrics_db_tx = db_tx.clone();
    let metrics_coins = coins.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(10));
        loop {
            interval.tick().await;
            let _ = metrics_db_tx.send(DbCommand::ComputeMetrics {
                coins: metrics_coins.clone(),
                tx: metrics_tx.clone(),
            });
        }
    });

    // --- Phase 3: everything else, throttled ---
    info!("Phase 3: background backfill for all coins and intervals...");
    let throttle = tokio::time::Duration::from_millis(1400);

    for (i, coin) in sorted_coins.iter().enumerate() {
        for interval in feeds::Interval::all() {
            // Skip if already fetched in phase 2
            if i < priority_count && priority_intervals.contains(interval) {
                continue;
            }

            let did_fetch = fetch_candle_gap(&feed, &db_tx, &tx, coin, *interval).await;
            if did_fetch {
                fetched_total += 1;
                tokio::time::sleep(throttle).await;
            } else {
                skipped_total += 1;
            }
        }
        let _ = tx.send(FeedMessage::Progress(feeds::BackfillProgress {
            coin: coin.name.clone(),
            current: i + 1,
            total,
            fetched: fetched_total,
            skipped: skipped_total,
            phase: "Background",
        }));
    }

    info!(
        "Backfill complete — fetched: {}, skipped: {}",
        fetched_total, skipped_total
    );

    // Prune old data
    let _ = db_tx.send(DbCommand::Prune);

    // Final metrics recompute with all data
    let _ = db_tx.send(DbCommand::ComputeMetrics {
        coins: coins.clone(),
        tx: tx.clone(),
    });
}

/// Fetch candles for a coin/interval, only filling the gap since last stored data.
/// Returns true if an API request was made (for throttling), false if skipped.
/// `display_name` is used for DB storage (e.g. "HYPE/SPOT"), `api_name` for the API call (e.g. "HYPE").
async fn fetch_candle_gap(
    feed: &HyperliquidFeed,
    db_tx: &mpsc::UnboundedSender<DbCommand>,
    tx: &mpsc::UnboundedSender<FeedMessage>,
    coin: &feeds::CoinInfo,
    interval: feeds::Interval,
) -> bool {
    let display_name = &coin.name;
    let api_name = coin.api_name();

    // Check what we already have (using display name for DB lookup)
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = db_tx.send(DbCommand::LatestCandleTime {
        coin: display_name.clone(),
        interval,
        reply: reply_tx,
    });

    let latest = reply_rx.await.ok().flatten();

    let since = match latest {
        Some(ts) => {
            let gap_secs = Utc::now().signed_duration_since(ts).num_seconds();
            if gap_secs < interval.duration_secs() {
                return false; // Data is fresh
            }
            Some(ts)
        }
        None => None,
    };

    let lookback = interval.max_candles();
    match feed.fetch_candles(api_name, interval, lookback, since).await {
        Ok(candles) if !candles.is_empty() => {
            // Re-tag candles with the display name for DB storage
            let tagged: Vec<feeds::Candle> = candles
                .into_iter()
                .map(|mut c| {
                    c.coin = display_name.clone();
                    c
                })
                .collect();
            let _ = db_tx.send(DbCommand::UpsertCandles(tagged.clone()));
            let _ = tx.send(FeedMessage::CandleBatch(tagged));
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!("Backfill failed for {} {}: {}", display_name, interval.as_str(), e);
        }
    }
    true
}

/// Compute metrics for all coins from the database and send to UI.
fn compute_and_send_metrics(
    db: &Database,
    coins: &[feeds::CoinInfo],
    tx: &mpsc::UnboundedSender<FeedMessage>,
) {
    let btc_candles_1h = db
        .get_candles("BTC", feeds::Interval::H1, 168)
        .unwrap_or_default();

    let btc_price = coins
        .iter()
        .find(|c| c.name == "BTC")
        .map(|c| c.price)
        .unwrap_or(0.0);

    let btc_current_price = if let Some(latest) = btc_candles_1h.last() {
        latest.close
    } else {
        btc_price
    };

    let btc_state = AnalysisEngine::compute_btc_state(&btc_candles_1h, btc_current_price);
    let _ = tx.send(FeedMessage::BtcStateUpdate(btc_state.clone()));

    // Pre-fetch BTC data per interval (used for correlation, beta, and vs BTC)
    let mut btc_returns: std::collections::HashMap<feeds::Interval, Vec<f64>> =
        std::collections::HashMap::new();
    let mut btc_last2: std::collections::HashMap<feeds::Interval, Vec<feeds::Candle>> =
        std::collections::HashMap::new();
    for interval in feeds::Interval::all() {
        let btc_candles = db
            .get_candles("BTC", *interval, interval.max_candles())
            .unwrap_or_default();
        btc_returns.insert(*interval, AnalysisEngine::log_returns(&btc_candles));
        // Cache last 2 candles for vs BTC calculation
        let last2: Vec<_> = btc_candles.iter().rev().take(2).rev().cloned().collect();
        btc_last2.insert(*interval, last2);
    }

    let mut all_metrics = Vec::new();

    for coin in coins {
        let mut metrics = CoinMetrics {
            coin: coin.name.clone(),
            is_perp: coin.is_perp,
            price: coin.price,
            change_24h_pct: coin.change_24h_pct,
            volume_24h: coin.volume_24h,
            open_interest: coin.open_interest,
            funding_rate: coin.funding_rate,
            ..Default::default()
        };

        for interval in feeds::Interval::all() {
            let candles = db
                .get_candles(&coin.name, *interval, interval.max_candles())
                .unwrap_or_default();

            if candles.len() < 2 {
                continue;
            }

            let vol = AnalysisEngine::volatility(&candles);
            metrics.volatility.insert(*interval, vol);

            let coin_returns = AnalysisEngine::log_returns(&candles);
            if let Some(btc_ret) = btc_returns.get(interval) {
                metrics
                    .btc_correlation
                    .insert(*interval, AnalysisEngine::correlation(&coin_returns, btc_ret));
                metrics
                    .btc_beta
                    .insert(*interval, AnalysisEngine::beta(&coin_returns, btc_ret));
            }

            // % change vs BTC for this timeframe (last candle period only)
            if let Some(btc_c) = btc_last2.get(interval) {
                let coin_last2: Vec<_> = candles.iter().rev().take(2).rev().cloned().collect();
                let vs = AnalysisEngine::relative_strength(&coin_last2, btc_c);
                metrics.vs_btc.insert(*interval, vs);
            }
        }

        if let Ok(latest) = db.get_candles(&coin.name, feeds::Interval::M5, 1) {
            if let Some(c) = latest.last() {
                metrics.price = c.close;
            }
        }

        all_metrics.push(metrics);
    }

    let _ = tx.send(FeedMessage::MetricsUpdate(all_metrics));
    let _ = tx.send(FeedMessage::DbSize(db.file_size()));
}
