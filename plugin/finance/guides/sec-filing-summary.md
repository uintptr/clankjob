# SEC filing summary

Adapted from `commands/filing.md`. The most recent SEC filing of one company: `10-K`
(annual, the default), `10-Q` (quarterly) or `8-K` (current report).

## Data

1. `sec_filings` for the ticker with `form` set: its date, period and EDGAR link.
2. `sec_filing_text` for the ticker with the same `form`. It is saved as a case file; read
   it with `read_file`, in parts. For a 10-K, `section` = `1A` (Risk Factors) and `7`
   (MD&A) keep it short; read `1` (Business) too for the overview.
3. `stock_data` with `data` = `info`, `income` (`freq` = `yearly` and `quarterly`),
   `balance` and `cashflow` (`yearly`). `sec_xbrl_facts` gives a figure exactly as filed
   when Yahoo's looks off.

Summarise risks and disclosures **from the filing's text**, quoting it briefly; never
from what such filings usually contain.

## Output

Write the summary as the answer (the `complete` summary, or a message to the owner).

## {TICKER} — {FORM} Summary

*Filing date: {date} · Data from SEC EDGAR and Yahoo Finance · as of {today}*

### Filing Metadata

- Type, period covered, filed date, link to SEC EDGAR

### Business Overview (10-K only)

- Core business description, key products/services, markets served
- Material changes vs prior year filing

### Financial Highlights

**Income Statement** (annual or most recent quarter)

| Metric           | Current Period | Prior Period | Change |
| ---------------- | -------------- | ------------ | ------ |
| Revenue          |                |              |        |
| Gross Profit     |                |              |        |
| Operating Income |                |              |        |
| Net Income       |                |              |        |
| EPS (diluted)    |                |              |        |

**Balance Sheet**

| Metric               | Current | Prior Year |
| -------------------- | ------- | ---------- |
| Total Assets         |         |            |
| Total Liabilities    |         |            |
| Shareholders' Equity |         |            |
| Cash & Equivalents   |         |            |
| Total Debt           |         |            |

**Cash Flow**

| Metric               | Current | Prior Year |
| -------------------- | ------- | ---------- |
| Operating Cash Flow  |         |            |
| Capital Expenditures |         |            |
| Free Cash Flow       |         |            |

### Key Disclosures & Risk Factors (10-K / 10-Q)

- The material risk factors, MD&A highlights and significant events the filing discloses

### Red Flags or Notable Items

- Unusual items, restatements, going concern language, significant litigation

*This analysis is for informational purposes only and does not constitute investment
advice. Verify all data independently and review the full filing on SEC EDGAR before
making financial decisions.*
