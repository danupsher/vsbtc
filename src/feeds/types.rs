use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Candle {
    pub coin: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub timestamp: DateTime<Utc>,
    pub interval: Interval,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Interval {
    M5,
    M15,
    H1,
    H4,
    D1,
    D7,
    D30,
}

impl Interval {
    pub fn as_str(&self) -> &'static str {
        match self {
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::H1 => "1h",
            Interval::H4 => "4h",
            Interval::D1 => "1d",
            Interval::D7 => "7d",
            Interval::D30 => "30d",
        }
    }

    pub fn as_hl_str(&self) -> &'static str {
        match self {
            Interval::M5 => "5m",
            Interval::M15 => "15m",
            Interval::H1 => "1h",
            Interval::H4 => "4h",
            Interval::D1 => "1d",
            Interval::D7 => "1w",
            Interval::D30 => "1M",
        }
    }

    pub fn all() -> &'static [Interval] {
        &[
            Interval::M5,
            Interval::M15,
            Interval::H1,
            Interval::H4,
            Interval::D1,
            Interval::D7,
            Interval::D30,
        ]
    }

    pub fn duration_secs(&self) -> i64 {
        match self {
            Interval::M5 => 300,
            Interval::M15 => 900,
            Interval::H1 => 3600,
            Interval::H4 => 14400,
            Interval::D1 => 86400,
            Interval::D7 => 604800,
            Interval::D30 => 2592000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoinInfo {
    pub name: String,
    pub is_perp: bool,
    pub price: f64,
    pub volume_24h: f64,
}

#[derive(Debug, Clone)]
pub struct BackfillProgress {
    pub coin: String,
    pub current: usize,
    pub total: usize,
}
