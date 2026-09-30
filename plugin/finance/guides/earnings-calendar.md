# Earnings calendar

Adapted from `commands/earnings-calendar.md`. Upcoming earnings dates for a list of
tickers. If the owner gives none, use a default tech watchlist: AAPL, MSFT, GOOGL, AMZN,
NVDA, META.

## Data

For each ticker, call `stock_data` with `data` = `earnings-dates` (`count` 4), `calendar`
and `fast-info`.

## Output

Write the calendar as the answer (the `complete` summary, or a message to the owner).

## Earnings Calendar

*Data from Yahoo Finance · as of {today}*

### Upcoming Earnings

Sorted by earnings date, ascending; only dates on or after today.

| Date | Ticker | Time                | EPS Est. | Rev Est. | Mkt Cap |
| ---- | ------ | ------------------- | -------- | -------- | ------- |
|      |        | Before/After Market |          |          |         |

*"Time" = before market open (BMO) or after market close (AMC) where available.*

### Next 30 Days — At a Glance

Grouped by week: which tickers report each week.

### Recent Past Earnings (last 30 days)

| Date | Ticker | EPS Actual | EPS Est. | Surprise % |
| ---- | ------ | ---------- | -------- | ---------- |
|      |        |            |          |            |

*This analysis is for informational purposes only and does not constitute investment
advice. Verify all data independently before making financial decisions.*
