Provide a concise company overview for the ticker symbol given as the argument (e.g. `/company AAPL`).

Run the following commands using `scripts/yf.py` and use the output as your data source:

```bash
scripts/yf.py $TICKER info
scripts/yf.py $TICKER fast-info
scripts/yf.py $TICKER recommendations
scripts/yf.py $TICKER price-targets
scripts/yf.py $TICKER news --count 5
```

Produce a structured summary with these sections:

## $TICKER — Company Overview
*Data from Yahoo Finance via yfinance · as of {today}*

**Business**
- Full legal name, sector, industry, country
- One-paragraph description of what the company does

**Market Snapshot**
| Metric | Value |
|--------|-------|
| Price | |
| Market Cap | |
| 52-week High / Low | |
| Average Volume (10d) | |
| Beta | |

**Valuation**
| Metric | Value |
|--------|-------|
| P/E (trailing) | |
| P/E (forward) | |
| P/S | |
| P/B | |
| EV/EBITDA | |

**Profitability**
| Metric | Value |
|--------|-------|
| Gross Margin | |
| Operating Margin | |
| Net Margin | |
| ROE | |
| ROA | |

**Balance Sheet Highlights**
- Total Cash, Total Debt, Debt/Equity

**Analyst Consensus**
- Current recommendation (Strong Buy / Buy / Hold / Sell)
- Price target: low / mean / high
- Number of analysts

**Recent News** (top 3 headlines with dates)

---
*This analysis is for informational purposes only and does not constitute investment advice. Verify all data independently before making financial decisions.*
