# DCF valuation

Adapted from `commands/dcf.md`. A discounted cash flow valuation of one company. The
owner may give assumptions (WACC, growth, terminal growth); use theirs over the defaults.

## Data

Call `stock_data` for the ticker with `data` = `info`, `fast-info`, `cashflow`, `income`
and `balance` (`freq` = `yearly`), `growth-estimates`, `earnings-estimate` and
`revenue-estimate`.

## Model

1. Free cash flow = operating cash flow − capital expenditure, for the last 3–4 years.
2. Call `dcf_model` with `fcfs` (that history, oldest first, in millions), `growth`
   (analyst estimates if available, else the historical CAGR), `wacc` (default 10),
   `terminal` (default 2.5), `net_debt` (total debt − cash, negative for net cash),
   `shares` (diluted, millions), `current_price` and `ticker`.
3. Use the model's output for the tables below, including its sensitivity table; do not
   redo its arithmetic by hand.

## Output

Write the valuation as the answer (the `complete` summary, or a message to the owner).

## {TICKER} — DCF Valuation

*Data from Yahoo Finance · as of {today}*

### Assumptions

| Parameter                          | Value   | Source / Rationale                                       |
| ---------------------------------- | ------- | -------------------------------------------------------- |
| WACC                               | %       | 10% default unless overridden; note how derived          |
| Revenue / FCF Growth Rate (Yr 1–5) | %       | analyst estimates if available, else 5yr historical CAGR |
| Terminal Growth Rate               | %       | 2.5% default unless overridden                           |
| Projection Period                  | 5 years |                                                          |
| Shares Outstanding                 |         | from fast-info                                           |
| Net Debt                           |         | from balance sheet                                       |

### Historical Free Cash Flow

The last 3 years of Operating Cash Flow, CapEx, Free Cash Flow (FCF = OCF − CapEx).

### Projected Free Cash Flow

| Year                 | FCF Projection | Discount Factor | PV of FCF |
| -------------------- | -------------- | --------------- | --------- |
| 1                    |                |                 |           |
| 2                    |                |                 |           |
| 3                    |                |                 |           |
| 4                    |                |                 |           |
| 5                    |                |                 |           |
| Terminal Value       |                |                 |           |
| **Enterprise Value** |                |                 |           |

### Intrinsic Value

| Item                          | Value |
| ----------------------------- | ----- |
| Enterprise Value              |       |
| Less: Net Debt                |       |
| Equity Value                  |       |
| Shares Outstanding            |       |
| **Intrinsic Value per Share** |       |
| Current Price                 |       |
| **Upside / Downside**         | %     |

### Sensitivity Table — Intrinsic Value per Share

WACC × terminal growth rate, from `dcf_model`.

### Key Risks to the Model

- 3–5 factors that could materially change the valuation (competition, margin
  compression, rate changes, etc.)

*DCF models are highly sensitive to assumptions. This analysis is for informational
purposes only and does not constitute investment advice. Verify all data independently
before making financial decisions.*
