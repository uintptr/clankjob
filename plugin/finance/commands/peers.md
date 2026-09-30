Compare the primary ticker against its sector peers. Usage: `/peers NVDA` or `/peers NVDA --vs AMD INTC QCOM TSM` to specify peers explicitly.

If peers are not provided, identify 4–6 relevant public competitors from your knowledge based on the company's sector and industry (retrieved from `info`).

Run the following commands for the primary ticker and each peer using `scripts/yf.py`:

```bash
# For each ticker in the comparison set:
scripts/yf.py $TICKER info
scripts/yf.py $TICKER fast-info
scripts/yf.py $TICKER income --freq trailing
scripts/yf.py $TICKER balance --freq yearly
scripts/yf.py $TICKER ratios   # (use info fields for ratio data)
```

## $PRIMARY — Peer Comparison
*Data from Yahoo Finance via yfinance · as of {today}*
*Peers: $PEER1, $PEER2, …*

### Valuation Multiples
| Ticker | Mkt Cap | P/E (fwd) | P/S | P/B | EV/EBITDA |
|--------|---------|-----------|-----|-----|-----------|
| | | | | | |

### Growth
| Ticker | Rev Growth (YoY) | EPS Growth (YoY) | 5yr Rev CAGR |
|--------|-----------------|-----------------|--------------|
| | | | |

### Profitability
| Ticker | Gross Margin | Op Margin | Net Margin | ROE | FCF Margin |
|--------|-------------|-----------|-----------|-----|-----------|
| | | | | | |

### Balance Sheet & Leverage
| Ticker | Cash | Total Debt | Net Debt/EBITDA | Current Ratio |
|--------|------|------------|----------------|---------------|
| | | | | |

### Analyst Sentiment
| Ticker | Recommendation | Price Target (mean) | Upside to Target |
|--------|---------------|--------------------|--------------------|
| | | | |

### Summary
- Highlight which metrics $PRIMARY leads or lags peers on
- Note any valuation premium or discount and whether it appears justified

---
*This analysis is for informational purposes only and does not constitute investment advice. Verify all data independently before making financial decisions.*
