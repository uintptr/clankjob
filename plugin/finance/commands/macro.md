Provide a current macro snapshot relevant to equity analysis. Optionally scoped to a ticker or sector: `/macro` (general) or `/macro AAPL` (contextualised to the company's sector).

If a ticker is provided, first run:

```bash
scripts/yf.py $TICKER info
```

Then fetch macro proxy data via yfinance using these benchmark tickers:

```bash
# US Equity indices
scripts/yf.py ^GSPC fast-info      # S&P 500
scripts/yf.py ^IXIC fast-info      # NASDAQ
scripts/yf.py ^DJI fast-info       # Dow Jones
scripts/yf.py ^RUT fast-info       # Russell 2000

# Rates & credit
scripts/yf.py ^TNX fast-info       # 10-yr Treasury yield
scripts/yf.py ^IRX fast-info       # 13-week T-bill yield
scripts/yf.py ^TYX fast-info       # 30-yr Treasury yield
scripts/yf.py HYG fast-info        # High-yield credit (proxy)
scripts/yf.py LQD fast-info        # Investment-grade credit (proxy)

# Commodities
scripts/yf.py GC=F fast-info       # Gold futures
scripts/yf.py CL=F fast-info       # Crude oil futures
scripts/yf.py BTC-USD fast-info    # Bitcoin (risk appetite proxy)

# Volatility & FX
scripts/yf.py ^VIX fast-info       # VIX volatility index
scripts/yf.py DX-Y.NYB fast-info   # US Dollar index
```

## Macro Snapshot
*Data from Yahoo Finance via yfinance · as of {today}*

### Equity Markets
| Index | Price | 1-Day Chg | YTD Chg |
|-------|-------|-----------|---------|
| S&P 500 | | | |
| NASDAQ | | | |
| Dow Jones | | | |
| Russell 2000 | | | |

### Interest Rates
| Instrument | Yield / Price | Change |
|-----------|--------------|--------|
| 13-wk T-bill | | |
| 10-yr Treasury | | |
| 30-yr Treasury | | |
| 10yr–2yr Spread | | (compute: signals inversion if negative) |

### Credit & Spreads
| ETF | Price | YTD | Notes |
|-----|-------|-----|-------|
| HYG (High Yield) | | | |
| LQD (IG Credit) | | | |

### Commodities & Risk Proxies
| Asset | Price | 1-Day Chg |
|-------|-------|-----------|
| Gold (GC=F) | | |
| Crude Oil (CL=F) | | |
| Bitcoin | | |

### Sentiment
| Indicator | Value | Signal |
|-----------|-------|--------|
| VIX | | Low <15 / Elevated >25 / Fear >35 |
| US Dollar Index | | |

### Macro Interpretation
- 2–3 paragraph narrative: current rate environment, risk-on/risk-off tone, key macro tailwinds and headwinds for equities
- If a ticker was provided: note how the current macro environment specifically affects $TICKER's sector

---
*This analysis is for informational purposes only and does not constitute investment advice. Verify all data independently before making financial decisions.*
