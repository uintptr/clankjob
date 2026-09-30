Summarise the most recent earnings release for the ticker given as the argument (e.g. `/earnings MSFT`).

Run the following commands using `scripts/yf.py` and use the output as your data source:

```bash
scripts/yf.py $TICKER earnings --freq quarterly
scripts/yf.py $TICKER earnings-history
scripts/yf.py $TICKER earnings-estimate
scripts/yf.py $TICKER revenue-estimate
scripts/yf.py $TICKER eps-trend
scripts/yf.py $TICKER eps-revisions
scripts/yf.py $TICKER income --freq quarterly
scripts/yf.py $TICKER calendar
scripts/yf.py $TICKER earnings-dates --count 8
```

Produce a structured earnings summary:

## $TICKER — Earnings Summary
*Data from Yahoo Finance via yfinance · as of {today}*

**Most Recent Quarter** (state the quarter, e.g. Q2 FY2025)
| Metric | Actual | Estimate | Surprise |
|--------|--------|----------|----------|
| EPS | | | |
| Revenue | | | |

**EPS Trend** — how consensus estimates have shifted over 7 / 30 / 60 / 90 days

**Next Earnings Date** — from calendar data

**Forward Estimates**
| Period | EPS Est. | Revenue Est. | # Analysts |
|--------|----------|--------------|-----------|
| Current Q | | | |
| Next Q | | | |
| FY Current | | | |
| FY Next | | | |

**EPS Revisions** (last 30 days: upgrades vs downgrades)

**Historical EPS Surprises** — table of last 4 quarters: estimated vs actual EPS and surprise %

**Income Statement Highlights** (last 2 quarters)
- Revenue, Gross Profit, Operating Income, Net Income, EPS Diluted

---
*This analysis is for informational purposes only and does not constitute investment advice. Verify all data independently before making financial decisions.*
