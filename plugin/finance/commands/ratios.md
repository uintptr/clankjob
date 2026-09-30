Compute and present key financial ratios for the ticker given as the argument (e.g. `/ratios GOOGL`).

Run the following commands using `scripts/yf.py` and use the output as your data source:

```bash
scripts/yf.py $TICKER info
scripts/yf.py $TICKER fast-info
scripts/yf.py $TICKER income --freq yearly
scripts/yf.py $TICKER income --freq trailing
scripts/yf.py $TICKER balance --freq yearly
scripts/yf.py $TICKER cashflow --freq yearly
```

Produce a structured ratio analysis. Compute any missing ratios from the raw statement data where possible.

## $TICKER — Ratio Analysis
*Data from Yahoo Finance via yfinance · as of {today}*

### Valuation
| Ratio | Value | Notes |
|-------|-------|-------|
| P/E (trailing) | | |
| P/E (forward) | | |
| PEG Ratio | | |
| Price / Sales | | |
| Price / Book | | |
| EV / EBITDA | | |
| EV / Revenue | | |

### Profitability
| Ratio | Value |
|-------|-------|
| Gross Margin | |
| Operating Margin | |
| EBITDA Margin | |
| Net Profit Margin | |
| Return on Equity (ROE) | |
| Return on Assets (ROA) | |
| Return on Invested Capital (ROIC) | |

### Efficiency
| Ratio | Value |
|-------|-------|
| Asset Turnover | |
| Inventory Turnover | |
| Receivables Turnover | |

### Liquidity & Leverage
| Ratio | Value |
|-------|-------|
| Current Ratio | |
| Quick Ratio | |
| Debt / Equity | |
| Net Debt / EBITDA | |
| Interest Coverage | |

### Per-Share Metrics
| Metric | Value |
|--------|-------|
| EPS (diluted, TTM) | |
| Free Cash Flow per Share | |
| Book Value per Share | |
| Dividend per Share | |
| Payout Ratio | |

**Interpretation** — briefly note any ratios that stand out as unusually high, low, or trending in a notable direction.

---
*This analysis is for informational purposes only and does not constitute investment advice. Verify all data independently before making financial decisions.*
