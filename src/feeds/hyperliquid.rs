use crate::feeds::types::*;
use chrono::{DateTime, Utc};
use futures_util::{SinkExt, StreamExt};
use reqwest::Client;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tracing::{error, info, warn};

const HL_REST_URL: &str = "https://api.hyperliquid.xyz/info";
const HL_WS_URL: &str = "wss://api.hyperliquid.xyz/ws";

pub struct HyperliquidFeed {
    client: Client,
}

/// Messages sent from the feed to the app.
#[derive(Debug, Clone)]
pub enum FeedMessage {
    /// List of all available coins on Hyperliquid.
    CoinList(Vec<CoinInfo>),
    /// A batch of historical candles from backfill.
    CandleBatch(Vec<Candle>),
    /// A live candle update from the WebSocket.
    LiveCandle(Candle),
    /// Backfill progress update.
    Progress(BackfillProgress),
    /// WebSocket connection status.
    Connected(bool),
    /// An error occurred.
    Error(String),
}

impl HyperliquidFeed {
    pub fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    /// Fetch all available coins (perps + spot) from Hyperliquid.
    pub async fn fetch_coin_list(&self) -> Result<Vec<CoinInfo>, String> {
        let mut coins = Vec::new();

        // Fetch perp metadata and market data
        let meta: Value = self
            .client
            .post(HL_REST_URL)
            .json(&json!({"type": "metaAndAssetCtxs"}))
            .send()
            .await
            .map_err(|e| format!("Failed to fetch perp meta: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Failed to parse perp meta: {e}"))?;

        if let (Some(universe), Some(contexts)) = (
            meta.get(0).and_then(|m| m.get("universe")).and_then(|u| u.as_array()),
            meta.get(1).and_then(|c| c.as_array()),
        ) {
            for (asset, ctx) in universe.iter().zip(contexts.iter()) {
                let name = asset
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let price = ctx
                    .get("markPx")
                    .and_then(|p| p.as_str())
                    .and_then(|p| p.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let volume_24h = ctx
                    .get("dayNtlVlm")
                    .and_then(|v| v.as_str())
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0);

                if !name.is_empty() {
                    coins.push(CoinInfo {
                        name,
                        is_perp: true,
                        price,
                        volume_24h,
                    });
                }
            }
        }

        // Fetch spot metadata and market data
        let spot_meta: Value = self
            .client
            .post(HL_REST_URL)
            .json(&json!({"type": "spotMetaAndAssetCtxs"}))
            .send()
            .await
            .map_err(|e| format!("Failed to fetch spot meta: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Failed to parse spot meta: {e}"))?;

        if let (Some(tokens), Some(contexts)) = (
            spot_meta
                .get(0)
                .and_then(|m| m.get("tokens"))
                .and_then(|t| t.as_array()),
            spot_meta.get(1).and_then(|c| c.as_array()),
        ) {
            for (token, ctx) in tokens.iter().zip(contexts.iter()) {
                let name = token
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or("")
                    .to_string();
                let price = ctx
                    .get("markPx")
                    .and_then(|p| p.as_str())
                    .and_then(|p| p.parse::<f64>().ok())
                    .unwrap_or(0.0);
                let volume_24h = ctx
                    .get("dayNtlVlm")
                    .and_then(|v| v.as_str())
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0);

                if !name.is_empty() && price > 0.0 {
                    coins.push(CoinInfo {
                        name,
                        is_perp: false,
                        price,
                        volume_24h,
                    });
                }
            }
        }

        info!("Fetched {} coins from Hyperliquid", coins.len());
        Ok(coins)
    }

    /// Fetch historical candles for a coin at a given interval.
    pub async fn fetch_candles(
        &self,
        coin: &str,
        interval: Interval,
        lookback_count: usize,
    ) -> Result<Vec<Candle>, String> {
        let now = Utc::now().timestamp_millis();
        let start = now - (interval.duration_secs() * lookback_count as i64 * 1000);

        let body = json!({
            "type": "candleSnapshot",
            "req": {
                "coin": coin,
                "interval": interval.as_hl_str(),
                "startTime": start,
                "endTime": now,
            }
        });

        let resp: Value = self
            .client
            .post(HL_REST_URL)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("Failed to fetch candles for {coin}: {e}"))?
            .json()
            .await
            .map_err(|e| format!("Failed to parse candles for {coin}: {e}"))?;

        let candles = resp
            .as_array()
            .unwrap_or(&vec![])
            .iter()
            .filter_map(|c| {
                let open = c.get("o")?.as_str()?.parse::<f64>().ok()?;
                let high = c.get("h")?.as_str()?.parse::<f64>().ok()?;
                let low = c.get("l")?.as_str()?.parse::<f64>().ok()?;
                let close = c.get("c")?.as_str()?.parse::<f64>().ok()?;
                let volume = c.get("v")?.as_str()?.parse::<f64>().ok()?;
                let ts = c.get("t")?.as_i64()?;
                let timestamp = DateTime::<Utc>::from_timestamp_millis(ts)?;

                Some(Candle {
                    coin: coin.to_string(),
                    open,
                    high,
                    low,
                    close,
                    volume,
                    timestamp,
                    interval,
                })
            })
            .collect();

        Ok(candles)
    }

    /// Run the full backfill for all coins across all intervals.
    /// Sends progress and candle data through the channel.
    pub async fn backfill(
        &self,
        coins: &[CoinInfo],
        tx: mpsc::UnboundedSender<FeedMessage>,
        lookback_count: usize,
    ) {
        let total = coins.len();

        for (i, coin) in coins.iter().enumerate() {
            let _ = tx.send(FeedMessage::Progress(BackfillProgress {
                coin: coin.name.clone(),
                current: i + 1,
                total,
            }));

            for interval in Interval::all() {
                match self.fetch_candles(&coin.name, *interval, lookback_count).await {
                    Ok(candles) if !candles.is_empty() => {
                        let _ = tx.send(FeedMessage::CandleBatch(candles));
                    }
                    Ok(_) => {}
                    Err(e) => {
                        warn!("Backfill failed for {} {}: {}", coin.name, interval.as_str(), e);
                    }
                }
            }
        }
    }

    /// Connect to the Hyperliquid WebSocket and stream live candle updates.
    pub async fn stream_live(
        coins: &[CoinInfo],
        tx: mpsc::UnboundedSender<FeedMessage>,
    ) {
        loop {
            info!("Connecting to Hyperliquid WebSocket...");

            let ws_result = connect_async(HL_WS_URL).await;
            let (mut ws, _) = match ws_result {
                Ok(conn) => {
                    let _ = tx.send(FeedMessage::Connected(true));
                    info!("WebSocket connected");
                    conn
                }
                Err(e) => {
                    error!("WebSocket connection failed: {e}");
                    let _ = tx.send(FeedMessage::Connected(false));
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                    continue;
                }
            };

            // Subscribe to candle updates for all coins
            for coin in coins {
                let sub = json!({
                    "method": "subscribe",
                    "subscription": {
                        "type": "candle",
                        "coin": coin.name,
                        "interval": "5m",
                    }
                });
                if let Err(e) = ws.send(Message::Text(sub.to_string().into())).await {
                    error!("Failed to subscribe to {}: {e}", coin.name);
                }
            }

            // Read messages
            while let Some(msg) = ws.next().await {
                match msg {
                    Ok(Message::Text(text)) => {
                        if let Ok(val) = serde_json::from_str::<Value>(&text) {
                            if let Some(candle) = parse_ws_candle(&val) {
                                let _ = tx.send(FeedMessage::LiveCandle(candle));
                            }
                        }
                    }
                    Ok(Message::Ping(data)) => {
                        let _ = ws.send(Message::Pong(data)).await;
                    }
                    Err(e) => {
                        error!("WebSocket error: {e}");
                        let _ = tx.send(FeedMessage::Connected(false));
                        break;
                    }
                    _ => {}
                }
            }

            warn!("WebSocket disconnected, reconnecting in 5s...");
            let _ = tx.send(FeedMessage::Connected(false));
            tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
        }
    }
}

fn parse_ws_candle(val: &Value) -> Option<Candle> {
    let data = val.get("data")?;
    let candle_data = data.get("data")?;

    let coin = data
        .get("s")?
        .as_str()?
        .trim_end_matches("/USD")
        .trim_end_matches("-PERP")
        .to_string();

    let open = candle_data.get("o")?.as_str()?.parse::<f64>().ok()?;
    let high = candle_data.get("h")?.as_str()?.parse::<f64>().ok()?;
    let low = candle_data.get("l")?.as_str()?.parse::<f64>().ok()?;
    let close = candle_data.get("c")?.as_str()?.parse::<f64>().ok()?;
    let volume = candle_data.get("v")?.as_str()?.parse::<f64>().ok()?;
    let ts = candle_data.get("t")?.as_i64()?;
    let timestamp = DateTime::<Utc>::from_timestamp_millis(ts)?;

    Some(Candle {
        coin,
        open,
        high,
        low,
        close,
        volume,
        timestamp,
        interval: Interval::M5,
    })
}
