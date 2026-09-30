# Macro snapshot

Adapted from `commands/macro.md`. The current macro picture for equity analysis, general
or for one company's sector.

## Data

If the owner names a company, first call `stock_data` with `data` = `info` for its sector.

Market proxies: `stock_data` with `data` = `fast-info` for each of

| Group             | Tickers                                                                                 |
| ----------------- | --------------------------------------------------------------------------------------- |
| US equity indices | `^GSPC` (S&P 500), `^IXIC` (NASDAQ), `^DJI` (Dow Jones), `^RUT` (Russell 2000)          |
| Rates and credit  | `^IRX` (13-week T-bill), `^TNX` (10-yr), `^TYX` (30-yr), `HYG` (high yield), `LQD` (IG) |
| Commodities       | `GC=F` (gold), `CL=F` (crude oil), `BTC-USD` (risk appetite)                            |
| Volatility and FX | `^VIX`, `DX-Y.NYB` (US dollar index)                                                    |

Official figures: `macro_snapshot` with `group` = `rates` and `inflation` (and `credit`,
`labour` when relevant). It needs a FRED API key; if it says the key is missing, go on
with the market proxies and say so. `fred_series` fetches any other series by id.

## Output

Write the snapshot as the answer (the `complete` summary, or a message to the owner).

## Macro Snapshot

*Data from Yahoo Finance and FRED · as of {today}*

### Equity Markets

| Index        | Price | 1-Day Chg | YTD Chg |
| ------------ | ----- | --------- | ------- |
| S&P 500      |       |           |         |
| NASDAQ       |       |           |         |
| Dow Jones    |       |           |         |
| Russell 2000 |       |           |         |

### Interest Rates

| Instrument      | Yield / Price | Change                                 |
| --------------- | ------------- | -------------------------------------- |
| 13-wk T-bill    |               |                                        |
| 10-yr Treasury  |               |                                        |
| 30-yr Treasury  |               |                                        |
| 10yr–2yr Spread |               | (FRED `T10Y2Y`; inversion if negative) |

### Inflation

| Measure  | YoY | Trend |
| -------- | --- | ----- |
| CPI      |     |       |
| Core CPI |     |       |
| PCE      |     |       |
| Core PCE |     |       |

### Credit & Spreads

| ETF              | Price | YTD | Notes |
| ---------------- | ----- | --- | ----- |
| HYG (High Yield) |       |     |       |
| LQD (IG Credit)  |       |     |       |

### Commodities & Risk Proxies

| Asset            | Price | 1-Day Chg |
| ---------------- | ----- | --------- |
| Gold (GC=F)      |       |           |
| Crude Oil (CL=F) |       |           |
| Bitcoin          |       |           |

### Sentiment

| Indicator       | Value | Signal                             |
| --------------- | ----- | ---------------------------------- |
| VIX             |       | Low \<15 / Elevated >25 / Fear >35 |
| US Dollar Index |       |                                    |

### Macro Interpretation

- 2–3 paragraphs: the rate environment, risk-on or risk-off tone, the main macro tailwinds
  and headwinds for equities
- If a company was named: how the current macro environment affects its sector

*This analysis is for informational purposes only and does not constitute investment
advice. Verify all data independently before making financial decisions.*
