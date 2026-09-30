Build a discounted cash flow (DCF) valuation for the ticker given as the argument (e.g. `/dcf AMZN`). Optionally accepts custom assumptions: `/dcf AMZN --wacc 9 --growth 15 --terminal 3`.

Run the following commands using `scripts/yf.py` and use the output as your data source:

```bash
scripts/yf.py $TICKER info
scripts/yf.py $TICKER fast-info
scripts/yf.py $TICKER cashflow --freq yearly
scripts/yf.py $TICKER income --freq yearly
scripts/yf.py $TICKER balance --freq yearly
scripts/yf.py $TICKER growth-estimates
scripts/yf.py $TICKER earnings-estimate
scripts/yf.py $TICKER revenue-estimate
```

## $TICKER — DCF Valuation
*Data from Yahoo Finance via yfinance · as of {today}*

### Assumptions
| Parameter | Value | Source / Rationale |
|-----------|-------|--------------------|
| WACC | % | (use 10% default unless overridden; note how derived) |
| Revenue / FCF Growth Rate (Yr 1–5) | % | (use analyst estimates if available, else 5yr historical CAGR) |
| Terminal Growth Rate | % | (use 2.5% default unless overridden) |
| Projection Period | 5 years | |
| Shares Outstanding | | from fast-info |
| Net Debt | | from balance sheet |

### Historical Free Cash Flow
Show the last 3 years of: Operating Cash Flow, CapEx, Free Cash Flow (FCF = OCF − CapEx).

### Projected Free Cash Flow
| Year | FCF Projection | Discount Factor | PV of FCF |
|------|---------------|-----------------|-----------|
| 1 | | | |
| 2 | | | |
| 3 | | | |
| 4 | | | |
| 5 | | | |
| Terminal Value | | | |
| **Enterprise Value** | | | |

### Intrinsic Value
| Item | Value |
|------|-------|
| Enterprise Value | |
| Less: Net Debt | |
| Equity Value | |
| Shares Outstanding | |
| **Intrinsic Value per Share** | |
| Current Price | |
| **Upside / Downside** | % |

### Sensitivity Table — Intrinsic Value per Share
Vary WACC (±2%) and Terminal Growth Rate (±1%) in a 3×3 grid.

### Key Risks to the Model
- List 3–5 factors that could materially change the valuation (competition, margin compression, rate changes, etc.)

---
*DCF models are highly sensitive to assumptions. This analysis is for informational purposes only and does not constitute investment advice. Verify all data independently before making financial decisions.*
