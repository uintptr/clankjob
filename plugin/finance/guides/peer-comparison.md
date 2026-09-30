# Peer comparison

Adapted from `commands/peers.md`. One company against its competitors. Use the peers the
owner names; otherwise pick 4–6 public competitors from the company's sector and industry
(in `info`) and say they are your choice.

## Data

For the company and each peer, call `stock_data` with `data` = `info` (most ratios are
there), `fast-info`, `income` (`freq` = `trailing`) and `balance` (`freq` = `yearly`).

## Output

Write the comparison as the answer (the `complete` summary, or a message to the owner).

## {TICKER} — Peer Comparison

*Data from Yahoo Finance · as of {today}*
*Peers: {PEER1}, {PEER2}, …*

### Valuation Multiples

| Ticker | Mkt Cap | P/E (fwd) | P/S | P/B | EV/EBITDA |
| ------ | ------- | --------- | --- | --- | --------- |
|        |         |           |     |     |           |

### Growth

| Ticker | Rev Growth (YoY) | EPS Growth (YoY) | 5yr Rev CAGR |
| ------ | ---------------- | ---------------- | ------------ |
|        |                  |                  |              |

### Profitability

| Ticker | Gross Margin | Op Margin | Net Margin | ROE | FCF Margin |
| ------ | ------------ | --------- | ---------- | --- | ---------- |
|        |              |           |            |     |            |

### Balance Sheet & Leverage

| Ticker | Cash | Total Debt | Net Debt/EBITDA | Current Ratio |
| ------ | ---- | ---------- | --------------- | ------------- |
|        |      |            |                 |               |

### Analyst Sentiment

| Ticker | Recommendation | Price Target (mean) | Upside to Target |
| ------ | -------------- | ------------------- | ---------------- |
|        |                |                     |                  |

### Summary

- Which metrics {TICKER} leads or lags its peers on
- Any valuation premium or discount, and whether it appears justified

*This analysis is for informational purposes only and does not constitute investment
advice. Verify all data independently before making financial decisions.*
