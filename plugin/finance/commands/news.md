Fetch and summarise the latest financial news headlines for one or more tickers or sector names.

**Supported argument forms:**
- Individual tickers: `/news AAPL`, `/news AAPL MSFT NVDA`
- Sector names: `/news tech`, `/news health finance`
- Mixed: `/news AAPL tech`
- No argument: broad market news (uses `SPY`)

**Sector → ETF proxy mapping** (use these tickers when the user names a sector):

| Sector keyword(s) | ETF proxy |
|-------------------|-----------|
| tech, technology | XLK |
| health, healthcare | XLV |
| finance, financials, banking | XLF |
| energy | XLE |
| consumer, retail | XLY |
| staples | XLP |
| industrials | XLI |
| materials | XLB |
| utilities | XLU |
| realestate, reit | XLRE |
| market, broad, (no arg) | SPY |

Resolve all inputs to ticker symbols using the mapping above, then run:

```bash
scripts/yf.py $TICKER news --count 10
```

for each resolved ticker.

Produce a structured digest with these sections:

## Financial News Digest — $TICKER
*Data from Yahoo Finance via yfinance · as of {today}*

For each ticker, output:

### $TICKER — Headlines
| # | Date (UTC) | Headline | Source |
|---|-----------|----------|--------|
| 1 | YYYY-MM-DD | [Title](url) | source domain |
| … | | | |

After the table, write a **2–3 sentence synthesis** summarising the dominant themes or sentiment across the headlines (e.g. product launches, earnings beats/misses, macro concerns, analyst actions, regulatory news).

If multiple tickers are requested, repeat the section for each, then add a final:

### Cross-Ticker Themes
- Bullet-point summary of any shared macro or sector-level narratives across all tickers.

---
*This digest is for informational purposes only and does not constitute investment advice. Verify all data independently before making financial decisions.*
