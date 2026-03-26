# vsBTC

Real-time crypto relative strength screener vs BTC — powered by Hyperliquid.

Track how every Hyperliquid perp performs relative to Bitcoin across multiple timeframes. Built for traders who want to see at a glance which coins are outperforming or underperforming BTC.

![Rust](https://img.shields.io/badge/Rust-000000?style=flat&logo=rust&logoColor=white)
![License](https://img.shields.io/badge/license-MIT-blue)

## What it does

- **vs BTC columns** — Shows each coin's % return minus BTC's % return across 5m, 15m, 1h, 4h, 1d, 7d, and 30d timeframes. Positive = outperforming BTC, negative = underperforming.
- **Live prices** — Real-time mid prices streamed via Hyperliquid WebSocket (allMids subscription).
- **Market data** — 24h change, volume, open interest, funding rates — refreshed every 60 seconds.
- **Volatility** — Per-timeframe volatility (standard deviation of log returns) for every coin.
- **BTC correlation & beta** — How tightly each coin tracks BTC and its sensitivity to BTC moves.
- **Sortable & filterable** — Click any column header to sort. Filter by name, minimum volume, or minimum volatility.
- **Click to trade** — Click any coin name to open it directly on Hyperliquid.

## How it works

1. On launch, fetches the full coin list and historical candles from the Hyperliquid REST API
2. Prioritizes BTC + top 20 coins by volume for fast initial load
3. Connects to the Hyperliquid WebSocket for live price streaming and 5m candle updates
4. Background backfills remaining coins and timeframes (rate-limited to stay within API limits)
5. Recomputes all metrics every 10 seconds using live prices + stored candle data
6. Stores candle history in a local SQLite database with automatic pruning

## Install

Download the latest release for your platform from the [Releases page](https://github.com/danupsher/vsbtc/releases).

| Platform | File |
|---|---|
| Linux x86_64 | `vsbtc-linux-x86_64.tar.gz` |
| Linux aarch64 | `vsbtc-linux-aarch64.tar.gz` |
| macOS x86_64 | `vsbtc-macos-x86_64.tar.gz` |
| macOS Apple Silicon | `vsbtc-macos-aarch64.tar.gz` |
| Windows x86_64 | `vsbtc-windows-x86_64.zip` |

Extract and run. No dependencies, no API keys needed — it connects directly to Hyperliquid's public API.

### macOS

After extracting, you may need to remove the quarantine attribute:

```bash
xattr -d com.apple.quarantine vsbtc
```

## Build from source

Requires Rust 1.85+ (edition 2024).

```bash
git clone https://github.com/danupsher/vsbtc.git
cd vsbtc
cargo build --release
./target/release/vsbtc
```

## Data storage

Candle data is stored in a SQLite database at your platform's standard data directory:

- **Linux**: `~/.local/share/vsbtc/data.db`
- **macOS**: `~/Library/Application Support/vsbtc/data.db`
- **Windows**: `C:\Users\<user>\AppData\Roaming\vsbtc\data.db`

Data is automatically pruned based on retention policies (3 days for 5m candles up to unlimited for monthly).

## License

MIT
