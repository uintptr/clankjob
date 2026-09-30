Show upcoming earnings dates for a watchlist of tickers. Usage: `/earnings-calendar AAPL MSFT GOOGL AMZN NVDA` (space-separated tickers). Falls back to a default tech watchlist if no tickers are provided.

For each ticker in the list, run:

```bash
scripts/yf.py $TICKER earnings-dates --count 4
scripts/yf.py $TICKER calendar
scripts/yf.py $TICKER fast-info
```

## Earnings Calendar
*Data from Yahoo Finance via yfinance · as of {today}*

### Upcoming Earnings
Sort all results by earnings date ascending. Only show dates on or after today.

| Date | Ticker | Time | EPS Est. | Rev Est. | Mkt Cap |
|------|--------|------|----------|----------|---------|
| | | Before/After Market | | | |

*"Time" = before market open (BMO) or after market close (AMC) where available.*

### Next 30 Days — At a Glance
Group by week. List which tickers report each week.

### Recent Past Earnings (last 30 days)
| Date | Ticker | EPS Actual | EPS Est. | Surprise % |
|------|--------|-----------|---------|-----------|
| | | | | |

---
*This analysis is for informational purposes only and does not constitute investment advice. Verify all data independently before making financial decisions.*
