# Earnings summary

Adapted from `commands/earnings.md`. The most recent earnings release of one company.

## Data

Call `stock_data` for the ticker with `data` = `earnings` and `income` (`freq` =
`quarterly`), `earnings-history`, `earnings-estimate`, `revenue-estimate`, `eps-trend`,
`eps-revisions`, `calendar`, and `earnings-dates` (`count` 8). For management's own words,
`sec_filings` (`form` = `8-K`) finds the earnings press release; for the call, see the
`earnings-call-analysis` guide if the YouTube transcripts plugin is loaded.

## Output

Write the summary as the answer (the `complete` summary, or a message to the owner).

## {TICKER} — Earnings Summary

*Data from Yahoo Finance · as of {today}*

**Most Recent Quarter** (state the quarter, e.g. Q2 FY2025)

| Metric  | Actual | Estimate | Surprise |
| ------- | ------ | -------- | -------- |
| EPS     |        |          |          |
| Revenue |        |          |          |

**EPS Trend** — how consensus estimates have shifted over 7 / 30 / 60 / 90 days

**Next Earnings Date** — from calendar data

**Forward Estimates**

| Period     | EPS Est. | Revenue Est. | # Analysts |
| ---------- | -------- | ------------ | ---------- |
| Current Q  |          |              |            |
| Next Q     |          |              |            |
| FY Current |          |              |            |
| FY Next    |          |              |            |

**EPS Revisions** (last 30 days: upgrades vs downgrades)

**Historical EPS Surprises** — the last 4 quarters: estimated vs actual EPS and surprise %

**Income Statement Highlights** (last 2 quarters)

- Revenue, Gross Profit, Operating Income, Net Income, EPS Diluted

*This analysis is for informational purposes only and does not constitute investment
advice. Verify all data independently before making financial decisions.*
