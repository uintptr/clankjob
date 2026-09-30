Summarise the most recent SEC filing for the ticker given as the argument (e.g. `/filing TSLA` or `/filing TSLA --type 10-Q`). Default filing type is 10-K (annual). Supported types: 10-K, 10-Q, 8-K.

Run the following commands using `scripts/yf.py`:

```bash
scripts/yf.py $TICKER sec-filings
scripts/yf.py $TICKER info
scripts/yf.py $TICKER income --freq yearly
scripts/yf.py $TICKER income --freq quarterly
scripts/yf.py $TICKER balance --freq yearly
scripts/yf.py $TICKER cashflow --freq yearly
```

From `sec-filings`, identify the most recent filing matching the requested type. Note its date and SEC EDGAR link if available.

## $TICKER — $FILING_TYPE Summary
*Filing date: {date} · Data from Yahoo Finance / SEC EDGAR · as of {today}*

### Filing Metadata
- Type, period covered, filed date, link to SEC EDGAR

### Business Overview (10-K only)
- Core business description, key products/services, markets served
- Material changes vs prior year filing

### Financial Highlights
Use the income statement, balance sheet, and cash flow data to populate:

**Income Statement** (annual or most recent quarter)
| Metric | Current Period | Prior Period | Change |
|--------|---------------|-------------|--------|
| Revenue | | | |
| Gross Profit | | | |
| Operating Income | | | |
| Net Income | | | |
| EPS (diluted) | | | |

**Balance Sheet**
| Metric | Current | Prior Year |
|--------|---------|-----------|
| Total Assets | | |
| Total Liabilities | | |
| Shareholders' Equity | | |
| Cash & Equivalents | | |
| Total Debt | | |

**Cash Flow**
| Metric | Current | Prior Year |
|--------|---------|-----------|
| Operating Cash Flow | | |
| Capital Expenditures | | |
| Free Cash Flow | | |

### Key Disclosures & Risk Factors (10-K / 10-Q)
- Summarise any material risk factors, management discussion highlights, or significant events disclosed. Use your knowledge of standard SEC filing structure to flag items typically found in MD&A.

### Red Flags or Notable Items
- Any unusual items, restatements, going concern language, or significant litigation

---
*This analysis is for informational purposes only and does not constitute investment advice. Verify all data independently and review the full filing on SEC EDGAR before making financial decisions.*
