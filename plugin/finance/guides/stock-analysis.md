# Full stock analysis

Adapted from `commands/analyse.md`. A complete, structured analysis of one company, for a
reporting period the owner names (e.g. `Q4 FY2025`) or, if they do not, the most recent
one in the earnings data.

## 1. Gather data

Make the calls independently of each other, as many at once as you can.

- `stock_data` for the ticker with `data` = `info`, `fast-info`, `income` (`freq` =
  `yearly` and `quarterly`), `balance` (`yearly`), `cashflow` (`yearly` and `quarterly`),
  `earnings-history`, `earnings-estimate`, `revenue-estimate`, `eps-trend`,
  `eps-revisions`, `growth-estimates`, `recommendations`, `price-targets`, `upgrades`,
  and `news` (`count` 10).
- `sec_filing_text` for the ticker (the latest 10-K), read with `read_file`; `section` =
  `1A` and `7` if the whole filing is too long.
- `macro_snapshot` with `group` = `rates`, then `inflation`. Without a FRED key, use the
  proxies of the `macro-snapshot` guide and say so.
- Peers: `stock_data` `info` for 3–5 competitors (see `peer-comparison`).
- If the case has the framework references as files (`jeff_bezos.md`,
  `warren_buffet.md`), read them before section 9. Without them, cite the principles by
  letter and year only where you are sure, and say the references were not available.

## 2. Run the DCF model

From the cash flow statements, take the last 3–4 years of free cash flow (operating cash
flow − capex). Call `dcf_model` twice, with `years` = 10, `fcfs` = that history, `net_debt`,
`shares`, `current_price` and `ticker`:

- **Base case** (conservative): `growth` = a base growth rate, `stage2_growth` = the
  terminal rate, `wacc`, `terminal`.
- **Bull case** (optimistic): `growth` = a bull growth rate, `stage2_growth` = the base
  rate, a lower `wacc`, the same `terminal`.

State every assumption and where it comes from.

## 3. Write the analysis

Write it as the answer (the `complete` summary, or a message to the owner), with these
nine sections in order.

# {TICKER} — {PERIOD} Analysis

*Data: Yahoo Finance, SEC EDGAR, FRED · as of {today}*

## 1. Company Overview

- Full legal name, sector, industry, headquarters
- One-paragraph business description
- **Market Snapshot** table: Price, Market Cap, 52-week High/Low, Beta, Avg Volume
- **Valuation** table: P/E trailing, P/E forward, P/S, P/B, EV/EBITDA
- **Profitability** table: Gross Margin, Operating Margin, Net Margin, ROE, ROA
- **Balance Sheet Highlights**: Cash, Total Debt, Net Cash/Debt, Debt/Equity
- **Analyst Consensus**: recommendation, price target (low / mean / high), # analysts

## 2. Earnings Summary

- Most recent quarter: reported EPS vs. estimate, revenue vs. estimate, surprise %
- **EPS Surprise History** table (last 4 quarters): Quarter | Estimate | Actual | Surprise %
- **Estimate Drift** table: how consensus EPS estimates have moved over 7d / 30d / 60d /
  90d
- **Forward Estimates** table: next quarter and next year EPS + revenue consensus

## 3. Ratio Analysis

- **Valuation Multiples** table: P/E, EV/EBITDA, EV/Revenue, P/FCF, P/B, P/S
- **Profitability Ratios** table: Gross Margin, EBITDA Margin, Net Margin, ROE, ROIC, ROA
- **Liquidity & Leverage** table: Current Ratio, Quick Ratio, Debt/Equity, Net
  Debt/EBITDA, Interest Coverage
- **Per-Share Metrics** table: EPS (TTM), FCF/share, Book Value/share, Dividend/share (if
  any)
- **Standout observations**: 2–3 bullet points on what the ratios reveal

## 4. DCF Valuation

- **Historical FCF Table**: Year | Operating CF | Capex | FCF | FCF Margin
- **Base Case** assumptions and implied share price vs. current price (upside/downside %)
- **Bull Case** assumptions and implied share price vs. current price
- **Sensitivity Table** from `dcf_model` (WACC × terminal growth rate)
- **Key Risks to DCF**: 3–5 bullets

## 5. SEC Filing Summary

- Filing type (10-K or 10-Q), filing date, period covered
- **Financial Highlights vs. Prior Year**: Revenue, Operating Income, Net Income, FCF —
  actual figures and % change
- **Material Changes**: segment shifts, new disclosures, accounting changes
- **Key Risk Factors**: the 5–7 most material risks cited by management
- **Red Flags**: anything that warrants closer scrutiny (channel stuffing, deferred
  revenue spikes, rising DSO, related-party transactions, auditor changes, etc.)

## 6. Peer Comparison

3–5 relevant peers, in four comparison tables:

- **Valuation**: P/E, EV/EBITDA, EV/Revenue, P/FCF
- **Growth & Profitability**: Revenue Growth YoY, Net Margin, ROE, FCF Margin
- **Balance Sheet**: Net Cash/Debt, Debt/Equity
- **Analyst Sentiment**: Recommendation, Mean Price Target, Implied Upside

Conclude with 2–3 bullets on where {TICKER} stands relative to peers.

## 7. Macro Snapshot

- **Equity Market Context**: S&P 500 level, YTD performance, VIX level, risk-on/risk-off
  read
- **Interest Rates**: Fed Funds rate, 10Y Treasury yield, real yield, yield curve shape
- **Inflation**: CPI YoY, PCE YoY, trend (accelerating / decelerating)
- **{TICKER}-Specific Macro Read**: how the current macro environment affects this
  company's sector, valuation multiple, or demand drivers (3–5 bullets)

## 8. News Digest

The 5–10 most recent headlines with dates, grouped by theme where several share a topic.
Close with 2–3 sentences on the dominant narrative in recent coverage.

## 9. Bezos & Buffett Framework Synthesis

*Every claim references a specific principle and year, grounded in `jeff_bezos.md` and
`warren_buffet.md` when the case has them.*

### Business Quality

**Buffett lens — Economic Moat**

- Is there a durable competitive advantage? Classify as Great / Good / Gruesome (Buffett
  2007).
- What is the source of the moat (brand, switching costs, network effects, cost
  advantage, intangibles)?
- Is management widening or eroding it?

**Bezos lens — Day 1 vs. Day 2**

- Assess customer obsession, decision speed, experimentation rate, and resistance to
  proxy metrics (Bezos 2016 — Day 2).
- Map the flywheel: where is it in its cycle? Is it self-reinforcing or stalling?

### Management

**Buffett lens — Owner-Orientation**

- Does management communicate like an owner or a promoter? (Compare tone to prior
  filings.)
- Capital allocation track record: buybacks, M&A, dividends — value-creating or
  value-destroying?

**Bezos lens — Working Backwards & Type 1/Type 2**

- Is product strategy customer-need-driven or capability-driven? (Bezos 2004 — working
  backwards)
- Are big bets treated as Type 2 (two-way door) decisions? Or is leadership
  over-centralising? (Bezos 2015)

### Capital Allocation

**Buffett lens — FCF vs. Reported Earnings**

- Compare owner earnings to GAAP net income. Quantify the gap. Flag any GAAP cosmetics.
- Is the business "Great": can it deploy incremental capital at high returns with low
  reinvestment needs?

**Bezos lens — Long-Term Math**

- Is management sacrificing compounding for near-term EPS management?
- Any visible investments with long payback horizons (R&D, new markets, infrastructure)?

### Valuation

**Buffett lens — Aesop's Birds in the Bush**

Apply: *"How certain am I of the cash flows, when will they arrive, and at what rate
should I discount them?"* (Buffett 2000)

- Base case intrinsic value range from the DCF.
- Margin of safety: how wide is the gap between current price and intrinsic value?

**Bezos lens — Platform Potential**

- Does the business have platform leverage (third-party builders, marketplace dynamics,
  API-style extensibility)?
- If so, how does it affect terminal value assumptions?

### Risks

5–7 material risks, tagged by type: Competitive / Regulatory / Macro / Execution /
Financial / Technology.

### Summary Scorecard

| Dimension                    | Rating | Notes |
| ---------------------------- | ------ | ----- |
| Business Quality (Moat)      | ★★★★☆  |       |
| Management Quality           | ★★★★☆  |       |
| Capital Allocation           | ★★★☆☆  |       |
| Growth Runway                | ★★★★☆  |       |
| Valuation (Margin of Safety) | ★★★☆☆  |       |
| **Overall**                  | ★★★★☆  |       |

**Buffett Verdict:** 1–2 sentences applying the Great/Good/Gruesome lens and the
birds-in-the-bush test.

> *The Buffett quote from `warren_buffet.md` that best fits this company*

**Bezos Verdict:** 1–2 sentences on Day 1 health, flywheel strength, and long-term
optionality.

> *The Bezos quote from `jeff_bezos.md` that best fits this company*

*This analysis is for informational purposes only and does not constitute investment
advice. Data sourced from Yahoo Finance, SEC EDGAR and FRED. Verify all data
independently before making financial decisions.*

## Rules

- **Never fill gaps from memory.** A figure no tool returned is `n/a`, with what would
  settle it; quotes come from the reference files or not at all.
- **Say where each number comes from** (Yahoo, the filing, FRED, the DCF model), and flag
  where sources disagree.
