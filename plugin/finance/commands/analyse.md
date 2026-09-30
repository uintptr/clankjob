Produce a full structured stock analysis for the ticker given as the argument (e.g. `/analyse AAPL` or `/analyse AAPL Q4 FY2025`).

The second argument is the reporting period label (e.g. `Q4 FY2025`, `FY2024`). If omitted, infer it from the most recent earnings data.

---

## Step 1 — Gather data (run all commands in parallel where possible)

```bash
scripts/yf.py $TICKER info
scripts/yf.py $TICKER fast-info
scripts/yf.py $TICKER income --freq yearly
scripts/yf.py $TICKER income --freq quarterly
scripts/yf.py $TICKER balance --freq yearly
scripts/yf.py $TICKER cashflow --freq yearly
scripts/yf.py $TICKER cashflow --freq quarterly
scripts/yf.py $TICKER earnings-history
scripts/yf.py $TICKER earnings-estimate
scripts/yf.py $TICKER revenue-estimate
scripts/yf.py $TICKER eps-trend
scripts/yf.py $TICKER eps-revisions
scripts/yf.py $TICKER growth-estimates
scripts/yf.py $TICKER recommendations
scripts/yf.py $TICKER price-targets
scripts/yf.py $TICKER upgrades
scripts/yf.py $TICKER news --count 10
scripts/sec.py fetch $TICKER --type 10-K --index 0
```

Also fetch macro context:
```bash
scripts/fred.py snapshot --group rates
scripts/fred.py snapshot --group inflation
```

Also read the framework reference files:
- `jeff_bezos.md`
- `warren_buffet.md`

---

## Step 2 — Run DCF model

From the cashflow data, extract the last 3–4 years of free cash flow (operating cash flow minus capex). Then run both a base case and bull case:

```bash
# Base case (conservative)
scripts/dcf.py --fcfs "$FCF_HISTORY" --growth $BASE_GROWTH --stage2-growth $TERMINAL_GROWTH \
               --wacc $WACC --terminal $TERMINAL_GROWTH --years 10 \
               --net-debt $NET_DEBT --shares $SHARES --current-price $PRICE --ticker $TICKER

# Bull case (optimistic)
scripts/dcf.py --fcfs "$FCF_HISTORY" --growth $BULL_GROWTH --stage2-growth $BASE_GROWTH \
               --wacc $WACC_LOW --terminal $TERMINAL_GROWTH --years 10 \
               --net-debt $NET_DEBT --shares $SHARES --current-price $PRICE --ticker $TICKER
```

---

## Step 3 — Write the analysis

Save the completed analysis as `analysis/$TICKER_$PERIOD.md` (e.g. `analysis/AAPL_Q4_FY2025.md`).

Produce the full analysis with these nine sections in order:

---

# $TICKER — $PERIOD Analysis
*Data: Yahoo Finance (yfinance), SEC EDGAR · as of {today}*

---

## 1. Company Overview

- Full legal name, sector, industry, headquarters
- One-paragraph business description
- **Market Snapshot** table: Price, Market Cap, 52-week High/Low, Beta, Avg Volume
- **Valuation** table: P/E trailing, P/E forward, P/S, P/B, EV/EBITDA
- **Profitability** table: Gross Margin, Operating Margin, Net Margin, ROE, ROA
- **Balance Sheet Highlights**: Cash, Total Debt, Net Cash/Debt, Debt/Equity
- **Analyst Consensus**: recommendation, price target (low / mean / high), # analysts

---

## 2. Earnings Summary

- Most recent quarter: reported EPS vs. estimate, revenue vs. estimate, surprise %
- **EPS Surprise History** table (last 4 quarters): Quarter | Estimate | Actual | Surprise %
- **Estimate Drift** table: how consensus EPS estimates have moved over 7d / 30d / 60d / 90d
- **Forward Estimates** table: next quarter and next year EPS + revenue consensus

---

## 3. Ratio Analysis

- **Valuation Multiples** table: P/E, EV/EBITDA, EV/Revenue, P/FCF, P/B, P/S
- **Profitability Ratios** table: Gross Margin, EBITDA Margin, Net Margin, ROE, ROIC, ROA
- **Liquidity & Leverage** table: Current Ratio, Quick Ratio, Debt/Equity, Net Debt/EBITDA, Interest Coverage
- **Per-Share Metrics** table: EPS (TTM), FCF/share, Book Value/share, Dividend/share (if any)
- **Standout observations**: 2–3 bullet points on what the ratios reveal

---

## 4. DCF Valuation

- **Historical FCF Table**: Year | Operating CF | Capex | FCF | FCF Margin
- **Base Case** assumptions and implied share price vs. current price (upside/downside %)
- **Bull Case** assumptions and implied share price vs. current price
- **Sensitivity Table** from dcf.py output (WACC × terminal growth rate)
- **Key Risks to DCF**: 3–5 bullets

---

## 5. SEC Filing Summary

- Filing type (10-K or 10-Q), filing date, period covered
- **Financial Highlights vs. Prior Year**: Revenue, Operating Income, Net Income, FCF — actual figures and % change
- **Material Changes**: segment shifts, new disclosures, accounting changes
- **Key Risk Factors**: 5–7 most material risks cited by management
- **Red Flags**: anything that warrants closer scrutiny (channel stuffing, deferred revenue spikes, rising DSO, related-party transactions, auditor changes, etc.)

---

## 6. Peer Comparison

Identify 3–5 relevant peers. Present four comparison tables:

- **Valuation**: P/E, EV/EBITDA, EV/Revenue, P/FCF
- **Growth & Profitability**: Revenue Growth YoY, Net Margin, ROE, FCF Margin
- **Balance Sheet**: Net Cash/Debt, Debt/Equity
- **Analyst Sentiment**: Recommendation, Mean Price Target, Implied Upside

Conclude with 2–3 bullets on where $TICKER stands relative to peers.

---

## 7. Macro Snapshot

- **Equity Market Context**: S&P 500 level, YTD performance, VIX level, risk-on/risk-off read
- **Interest Rates**: Fed Funds rate, 10Y Treasury yield, real yield, yield curve shape
- **Inflation**: CPI YoY, PCE YoY, trend (accelerating / decelerating)
- **$TICKER-Specific Macro Read**: how the current macro environment affects this company's sector, valuation multiple, or demand drivers (3–5 bullets)

---

## 8. News Digest

List the 5–10 most recent headlines with dates. Group by theme if multiple headlines share a topic. Close with 2–3 sentences on the dominant narrative in recent coverage.

---

## 9. Bezos & Buffett Framework Synthesis

*Ground all citations in `jeff_bezos.md` and `warren_buffet.md`. Every claim must reference a specific principle and year.*

### Business Quality

**Buffett lens — Economic Moat**
- Is there a durable competitive advantage? Classify as Great / Good / Gruesome (Buffett 2007).
- What is the source of the moat (brand, switching costs, network effects, cost advantage, intangibles)?
- Is management widening or eroding it?

**Bezos lens — Day 1 vs. Day 2**
- Assess customer obsession, decision speed, experimentation rate, and resistance to proxy metrics (Bezos 2016 — Day 2).
- Map the flywheel: where is it in its cycle? Is it self-reinforcing or stalling?

### Management

**Buffett lens — Owner-Orientation**
- Does management communicate like an owner or a promoter? (Compare tone to prior filings.)
- Capital allocation track record: buybacks, M&A, dividends — value-creating or value-destroying?

**Bezos lens — Working Backwards & Type 1/Type 2**
- Is product strategy customer-need-driven or capability-driven? (Bezos 2004 — working backwards)
- Are big bets treated as Type 2 (two-way door) decisions? Or is leadership over-centralising? (Bezos 2015)

### Capital Allocation

**Buffett lens — FCF vs. Reported Earnings**
- Compare owner earnings to GAAP net income. Quantify the gap. Flag any GAAP cosmetics.
- Is the business "Great": can it deploy incremental capital at high returns with low reinvestment needs?

**Bezos lens — Long-Term Math**
- Is management sacrificing compounding for near-term EPS management?
- Any visible investments with long payback horizons (R&D, new markets, infrastructure)?

### Valuation

**Buffett lens — Aesop's Birds in the Bush**
Apply: *"How certain am I of the cash flows, when will they arrive, and at what rate should I discount them?"* (Buffett 2000)
- Base case intrinsic value range from DCF.
- Margin of safety: how wide is the gap between current price and intrinsic value?

**Bezos lens — Platform Potential**
- Does the business have platform leverage (third-party builders, marketplace dynamics, API-style extensibility)?
- If so, how does it affect terminal value assumptions?

### Risks

List 5–7 material risks, tagged by type: Competitive / Regulatory / Macro / Execution / Financial / Technology.

---

### Summary Scorecard

| Dimension | Rating | Notes |
|-----------|--------|-------|
| Business Quality (Moat) | ★★★★☆ | |
| Management Quality | ★★★★☆ | |
| Capital Allocation | ★★★☆☆ | |
| Growth Runway | ★★★★☆ | |
| Valuation (Margin of Safety) | ★★★☆☆ | |
| **Overall** | ★★★★☆ | |

**Buffett Verdict:** [1–2 sentences applying the Great/Good/Gruesome lens and birds-in-bush test]
> *Closing Buffett quote from `warren_buffet.md` that best fits this company*

**Bezos Verdict:** [1–2 sentences on Day 1 health, flywheel strength, and long-term optionality]
> *Closing Bezos quote from `jeff_bezos.md` that best fits this company*

---
*This analysis is for informational purposes only and does not constitute investment advice. Data sourced from Yahoo Finance (yfinance) and SEC EDGAR. Verify all data independently before making financial decisions.*
