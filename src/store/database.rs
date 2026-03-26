use crate::feeds::types::*;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection};
use std::path::PathBuf;
use tracing::info;

pub struct Database {
    conn: Connection,
}

impl Database {
    pub fn open() -> Result<Self, String> {
        let path = Self::db_path()?;

        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("Failed to create data directory: {e}"))?;
        }

        let conn =
            Connection::open(&path).map_err(|e| format!("Failed to open database: {e}"))?;

        let db = Self { conn };
        db.init_tables()?;

        info!("Database opened at {}", path.display());
        Ok(db)
    }

    fn db_path() -> Result<PathBuf, String> {
        let data_dir =
            dirs::data_dir().ok_or_else(|| "Could not determine data directory".to_string())?;
        Ok(data_dir.join("cryptoscreener").join("data.db"))
    }

    fn init_tables(&self) -> Result<(), String> {
        self.conn
            .execute_batch(
                "
            CREATE TABLE IF NOT EXISTS candles (
                coin TEXT NOT NULL,
                interval TEXT NOT NULL,
                timestamp INTEGER NOT NULL,
                open REAL NOT NULL,
                high REAL NOT NULL,
                low REAL NOT NULL,
                close REAL NOT NULL,
                volume REAL NOT NULL,
                PRIMARY KEY (coin, interval, timestamp)
            );

            CREATE TABLE IF NOT EXISTS coins (
                name TEXT PRIMARY KEY,
                is_perp INTEGER NOT NULL,
                last_price REAL NOT NULL DEFAULT 0,
                volume_24h REAL NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT 0
            );

            CREATE INDEX IF NOT EXISTS idx_candles_coin_interval
                ON candles(coin, interval, timestamp);
            ",
            )
            .map_err(|e| format!("Failed to create tables: {e}"))?;

        Ok(())
    }

    pub fn upsert_candles(&self, candles: &[Candle]) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Transaction failed: {e}"))?;

        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO candles (coin, interval, timestamp, open, high, low, close, volume)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                )
                .map_err(|e| format!("Prepare failed: {e}"))?;

            for c in candles {
                stmt.execute(params![
                    c.coin,
                    c.interval.as_str(),
                    c.timestamp.timestamp(),
                    c.open,
                    c.high,
                    c.low,
                    c.close,
                    c.volume,
                ])
                .map_err(|e| format!("Insert failed: {e}"))?;
            }
        }

        tx.commit().map_err(|e| format!("Commit failed: {e}"))?;
        Ok(())
    }

    pub fn upsert_coins(&self, coins: &[CoinInfo]) -> Result<(), String> {
        let tx = self
            .conn
            .unchecked_transaction()
            .map_err(|e| format!("Transaction failed: {e}"))?;

        {
            let mut stmt = tx
                .prepare_cached(
                    "INSERT OR REPLACE INTO coins (name, is_perp, last_price, volume_24h, updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                )
                .map_err(|e| format!("Prepare failed: {e}"))?;

            let now = Utc::now().timestamp();
            for c in coins {
                stmt.execute(params![c.name, c.is_perp as i32, c.price, c.volume_24h, now])
                    .map_err(|e| format!("Insert failed: {e}"))?;
            }
        }

        tx.commit().map_err(|e| format!("Commit failed: {e}"))?;
        Ok(())
    }

    /// Get candles for a coin at a specific interval, ordered by time.
    pub fn get_candles(
        &self,
        coin: &str,
        interval: Interval,
        limit: usize,
    ) -> Result<Vec<Candle>, String> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT coin, interval, timestamp, open, high, low, close, volume
                 FROM candles
                 WHERE coin = ?1 AND interval = ?2
                 ORDER BY timestamp DESC
                 LIMIT ?3",
            )
            .map_err(|e| format!("Prepare failed: {e}"))?;

        let candles = stmt
            .query_map(params![coin, interval.as_str(), limit], |row| {
                let ts: i64 = row.get(2)?;
                let interval_str: String = row.get(1)?;
                Ok(Candle {
                    coin: row.get(0)?,
                    interval: parse_interval(&interval_str),
                    timestamp: DateTime::<Utc>::from_timestamp(ts, 0).unwrap_or_default(),
                    open: row.get(3)?,
                    high: row.get(4)?,
                    low: row.get(5)?,
                    close: row.get(6)?,
                    volume: row.get(7)?,
                })
            })
            .map_err(|e| format!("Query failed: {e}"))?
            .filter_map(|r| r.ok())
            .collect::<Vec<_>>();

        Ok(candles)
    }

    /// Get the latest candle timestamp for a coin/interval combo.
    pub fn latest_candle_time(
        &self,
        coin: &str,
        interval: Interval,
    ) -> Result<Option<DateTime<Utc>>, String> {
        let mut stmt = self
            .conn
            .prepare_cached(
                "SELECT MAX(timestamp) FROM candles WHERE coin = ?1 AND interval = ?2",
            )
            .map_err(|e| format!("Prepare failed: {e}"))?;

        let result: Option<i64> = stmt
            .query_row(params![coin, interval.as_str()], |row| row.get(0))
            .map_err(|e| format!("Query failed: {e}"))?;

        Ok(result.and_then(|ts| DateTime::<Utc>::from_timestamp(ts, 0)))
    }

    /// Get database file size in bytes.
    pub fn file_size(&self) -> u64 {
        Self::db_path()
            .ok()
            .and_then(|p| std::fs::metadata(p).ok())
            .map(|m| m.len())
            .unwrap_or(0)
    }
}

fn parse_interval(s: &str) -> Interval {
    match s {
        "5m" => Interval::M5,
        "15m" => Interval::M15,
        "1h" => Interval::H1,
        "4h" => Interval::H4,
        "1d" => Interval::D1,
        "7d" => Interval::D7,
        "30d" => Interval::D30,
        _ => Interval::M5,
    }
}
