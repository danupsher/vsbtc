use crate::feeds::types::*;
use std::collections::HashMap;

/// Computed metrics for a single coin.
#[derive(Debug, Clone, Default)]
pub struct CoinMetrics {
    pub coin: String,
    pub is_perp: bool,
    pub price: f64,
    pub change_24h_pct: f64,
    pub volume_24h: f64,
    pub open_interest: f64,
    pub funding_rate: f64,
    /// Volatility per timeframe (normalized std dev of returns).
    pub volatility: HashMap<Interval, f64>,
    /// Correlation with BTC per timeframe (-1.0 to 1.0).
    pub btc_correlation: HashMap<Interval, f64>,
    /// Beta vs BTC per timeframe.
    pub btc_beta: HashMap<Interval, f64>,
    /// Relative strength vs BTC (current period return minus BTC return).
    pub relative_strength: f64,
    /// Composite score (higher = more likely to pump on BTC recovery).
    pub score: f64,
}

/// BTC market state.
#[derive(Debug, Clone, Default)]
pub struct BtcState {
    pub price: f64,
    pub change_24h_pct: f64,
    pub drawdown_pct: f64,
    pub local_high: f64,
    pub is_dipping: bool,
    pub dip_duration_secs: i64,
}

pub struct AnalysisEngine;

impl AnalysisEngine {
    /// Calculate volatility as the standard deviation of log returns.
    pub fn volatility(candles: &[Candle]) -> f64 {
        if candles.len() < 2 {
            return 0.0;
        }

        let returns: Vec<f64> = candles
            .windows(2)
            .filter_map(|w| {
                if w[0].close > 0.0 && w[1].close > 0.0 {
                    Some((w[1].close / w[0].close).ln())
                } else {
                    None
                }
            })
            .collect();

        if returns.is_empty() {
            return 0.0;
        }

        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let variance =
            returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / returns.len() as f64;
        variance.sqrt()
    }

    /// Calculate Pearson correlation between two return series.
    pub fn correlation(returns_a: &[f64], returns_b: &[f64]) -> f64 {
        let n = returns_a.len().min(returns_b.len());
        if n < 2 {
            return 0.0;
        }

        let a = &returns_a[..n];
        let b = &returns_b[..n];

        let mean_a = a.iter().sum::<f64>() / n as f64;
        let mean_b = b.iter().sum::<f64>() / n as f64;

        let mut cov = 0.0;
        let mut var_a = 0.0;
        let mut var_b = 0.0;

        for i in 0..n {
            let da = a[i] - mean_a;
            let db = b[i] - mean_b;
            cov += da * db;
            var_a += da * da;
            var_b += db * db;
        }

        if var_a == 0.0 || var_b == 0.0 {
            return 0.0;
        }

        cov / (var_a.sqrt() * var_b.sqrt())
    }

    /// Calculate beta (sensitivity to BTC moves).
    /// Beta = covariance(coin, btc) / variance(btc)
    pub fn beta(coin_returns: &[f64], btc_returns: &[f64]) -> f64 {
        let n = coin_returns.len().min(btc_returns.len());
        if n < 2 {
            return 1.0;
        }

        let a = &coin_returns[..n];
        let b = &btc_returns[..n];

        let mean_a = a.iter().sum::<f64>() / n as f64;
        let mean_b = b.iter().sum::<f64>() / n as f64;

        let mut cov = 0.0;
        let mut var_b = 0.0;

        for i in 0..n {
            let da = a[i] - mean_a;
            let db = b[i] - mean_b;
            cov += da * db;
            var_b += db * db;
        }

        if var_b == 0.0 {
            return 1.0;
        }

        cov / var_b
    }

    /// Extract log returns from candles (sorted oldest to newest).
    pub fn log_returns(candles: &[Candle]) -> Vec<f64> {
        candles
            .windows(2)
            .filter_map(|w| {
                if w[0].close > 0.0 && w[1].close > 0.0 {
                    Some((w[1].close / w[0].close).ln())
                } else {
                    None
                }
            })
            .collect()
    }

    /// Calculate relative strength: coin return minus BTC return over the same period.
    pub fn relative_strength(coin_candles: &[Candle], btc_candles: &[Candle]) -> f64 {
        let coin_return = Self::period_return(coin_candles);
        let btc_return = Self::period_return(btc_candles);
        coin_return - btc_return
    }

    /// Simple return over a candle series (first close to last close).
    fn period_return(candles: &[Candle]) -> f64 {
        if candles.len() < 2 {
            return 0.0;
        }
        let first = candles.first().unwrap().close;
        let last = candles.last().unwrap().close;
        if first == 0.0 {
            return 0.0;
        }
        (last - first) / first
    }

    /// Compute BTC state from recent candles.
    pub fn compute_btc_state(candles_1h: &[Candle], current_price: f64) -> BtcState {
        if candles_1h.is_empty() {
            return BtcState {
                price: current_price,
                ..Default::default()
            };
        }

        // Find local high (highest close in recent candles)
        let local_high = candles_1h
            .iter()
            .map(|c| c.high)
            .fold(f64::MIN, f64::max);

        let drawdown_pct = if local_high > 0.0 {
            ((current_price - local_high) / local_high) * 100.0
        } else {
            0.0
        };

        // 24h change
        let change_24h_pct = if candles_1h.len() >= 24 {
            let price_24h_ago = candles_1h[candles_1h.len().saturating_sub(24)].close;
            if price_24h_ago > 0.0 {
                ((current_price - price_24h_ago) / price_24h_ago) * 100.0
            } else {
                0.0
            }
        } else {
            0.0
        };

        // Consider it a "dip" if drawdown > 2%
        let is_dipping = drawdown_pct < -2.0;

        // Count consecutive dipping candles from the end
        let mut dip_candles = 0i64;
        for candle in candles_1h.iter().rev() {
            if candle.close < local_high * 0.98 {
                dip_candles += 1;
            } else {
                break;
            }
        }

        BtcState {
            price: current_price,
            change_24h_pct,
            drawdown_pct,
            local_high,
            is_dipping,
            dip_duration_secs: dip_candles * 3600,
        }
    }

    /// Compute composite score for ranking.
    /// Higher = more volatile, liquid, outperforming BTC with high beta.
    pub fn composite_score(metrics: &CoinMetrics) -> f64 {
        let avg_vol = if metrics.volatility.is_empty() {
            0.0
        } else {
            metrics.volatility.values().sum::<f64>() / metrics.volatility.len() as f64
        };

        let vol_factor = if metrics.volume_24h > 0.0 {
            (metrics.volume_24h.ln() / 20.0).clamp(0.0, 1.0)
        } else {
            0.0
        };

        let rs_factor = (metrics.relative_strength * 10.0).clamp(-1.0, 1.0);

        let avg_beta = if metrics.btc_beta.is_empty() {
            1.0
        } else {
            metrics.btc_beta.values().sum::<f64>() / metrics.btc_beta.len() as f64
        };
        let beta_factor = (avg_beta / 2.0).clamp(0.0, 1.0);

        let score = (avg_vol * 30.0) + (vol_factor * 20.0) + (rs_factor * 35.0) + (beta_factor * 15.0);

        score.clamp(0.0, 100.0)
    }
}
