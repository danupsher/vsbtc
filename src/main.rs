mod analysis;
mod feeds;
mod store;
mod ui;

use feeds::hyperliquid::HyperliquidFeed;
use feeds::FeedMessage;
use store::Database;
use tokio::sync::mpsc;
use tracing::info;

fn main() -> eframe::Result<()> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    // Create async runtime for the data backend
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");

    // Channel for feed -> UI communication
    let (tx, rx) = mpsc::unbounded_channel::<FeedMessage>();

    // Spawn the async data pipeline
    rt.spawn(async move {
        run_data_pipeline(tx).await;
    });

    // Launch the GUI
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1400.0, 800.0])
            .with_min_inner_size([1000.0, 600.0])
            .with_title("CryptoScreener"),
        ..Default::default()
    };

    eframe::run_native(
        "CryptoScreener",
        options,
        Box::new(|cc| Ok(Box::new(ui::CryptoScreenerApp::new(cc, rx)))),
    )
}

async fn run_data_pipeline(tx: mpsc::UnboundedSender<FeedMessage>) {
    // Open database
    let db = match Database::open() {
        Ok(db) => db,
        Err(e) => {
            let _ = tx.send(FeedMessage::Error(format!("Database error: {e}")));
            return;
        }
    };

    let feed = HyperliquidFeed::new();

    // Fetch coin list
    info!("Fetching coin list...");
    let coins = match feed.fetch_coin_list().await {
        Ok(coins) => {
            let _ = db.upsert_coins(&coins);
            let _ = tx.send(FeedMessage::CoinList(coins.clone()));
            coins
        }
        Err(e) => {
            let _ = tx.send(FeedMessage::Error(format!("Failed to fetch coins: {e}")));
            return;
        }
    };

    // Backfill historical candles
    info!("Starting backfill for {} coins...", coins.len());
    let backfill_tx = tx.clone();
    feed.backfill(&coins, backfill_tx, 200).await;
    info!("Backfill complete");

    // Start live WebSocket feed
    info!("Starting live feed...");
    HyperliquidFeed::stream_live(&coins, tx).await;
}
