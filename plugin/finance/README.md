# Finance research plugin

Lets cases research stocks and markets. Ask a case for a company overview, a DCF, an
earnings summary, a filing's risk factors, what Berkshire bought last quarter or the macro
picture, and the agent pulls the data with these tools and writes it up following the
guides.

It is a **command plugin** (`runtime = "command"`, design §9.9): `plugin.toml` turns the
scripts in `scripts/` into tools, with no plugin protocol to implement.

| Tool                         | What it does                                                              | Source           |
| ---------------------------- | ------------------------------------------------------------------------- | ---------------- |
| `stock_data`                 | Quotes, profile, statements, estimates, holders, dividends, options, news | Yahoo Finance    |
| `stock_screen`               | Screens stocks or funds by conditions                                     | Yahoo Finance    |
| `stock_screen_preset`        | Ready-made screens: value, growth, dividend, quality, deep value, …       | Yahoo Finance    |
| `sp500_pe`                   | The S&P 500 ranked by P/E (slow: 500 tickers)                             | Yahoo, Wikipedia |
| `sec_filings`                | A company's recent filings                                                | SEC EDGAR        |
| `sec_filing_text`            | A filing's text, or one item of it; saved as a case file                  | SEC EDGAR        |
| `sec_xbrl_facts`             | Figures exactly as filed (XBRL)                                           | SEC EDGAR        |
| `sec_full_text_search`       | Full-text search of all filings since 2001                                | SEC EDGAR        |
| `institutional_holdings_13f` | 13F holdings, quarter-over-quarter changes, crowding across managers      | SEC EDGAR        |
| `macro_snapshot`             | Rates, inflation, growth, labour, credit, housing, money, sentiment       | FRED             |
| `fred_series`                | Any FRED series by id                                                     | FRED             |
| `fred_search`                | Finds FRED series by keyword                                              | FRED             |
| `dcf_model`                  | DCF valuation with a sensitivity table                                    | computed         |
| `pairs_test`                 | Cointegration test of two securities (pairs trade)                        | Yahoo Finance    |
| `leveraged_etf_decay`        | A leveraged ETF's volatility decay against its underlying                 | Yahoo Finance    |

| Guide                | Read by the agent when…                                                       |
| -------------------- | ----------------------------------------------------------------------------- |
| `stock-analysis`     | the owner wants a full analysis of a company (with Bezos & Buffett synthesis) |
| `company-overview`   | a short overview of a company                                                 |
| `dcf-valuation`      | a DCF valuation                                                               |
| `earnings-summary`   | the latest earnings of a company                                              |
| `earnings-calendar`  | upcoming earnings dates for a list of tickers                                 |
| `sec-filing-summary` | a summary of a 10-K, 10-Q or 8-K                                              |
| `macro-snapshot`     | the macro picture, general or for one company                                 |
| `news-digest`        | recent news for tickers or sectors                                            |
| `peer-comparison`    | a company against its competitors                                             |
| `ratio-analysis`     | a company's financial ratios                                                  |

Earnings calls on YouTube are the `youtube_transcribe` plugin's job (its
`earnings-call-analysis` guide); the guides here point to it.

## Setup

- Needs [`uv`](https://docs.astral.sh/uv/) and `python3` on the server's `PATH`. The
  first call of each script installs its dependencies (`yfinance`, `pandas`, `httpx`,
  `statsmodels`, …), which takes a minute; `scripts/13f.py` uses the standard library
  only.

- Works without configuration, except FRED. For `macro_snapshot`, `fred_series` and
  `fred_search`, get a free [FRED API key](https://fred.stlouisfed.org/docs/api/api_key.html),
  copy `config.example.toml` to `config.toml` and pass it through `[env]`:

  ```toml
  [env]
  FRED_API_KEY = { env = "FRED_API_KEY" }
  SEC_USER_AGENT = "Your Name you@example.com"
  ```

  SEC EDGAR asks callers to identify themselves; set `SEC_USER_AGENT` to your name and
  address so requests are not rate-limited.

- Run `./check_config.py` (after `export FRED_API_KEY=…` when the config uses `{ env = … }`)
  to check it on this machine: the programs, the scripts, and whether Yahoo Finance, SEC
  EDGAR and FRED answer. A passing run:

  ```
    ok   uv on PATH
    ok   python3 on PATH
    ok   9 scripts are executable
    ok   Yahoo Finance quote for AAPL (scripts/yf.py)
    ok   SEC EDGAR filings for AAPL (scripts/sec.py)
    ok   SEC EDGAR 13F filings (scripts/13f.py)
    ok   FRED observations (scripts/fred.py)
    ok   DCF model runs (scripts/dcf.py)
  ```

  A missing `FRED_API_KEY` or `SEC_USER_AGENT` is a `warn`, not a failure.

The server loads it from `plugins_dir` and reloads it when these files change. The
Plugins page lists its tools and guides.

## Standalone use

Every script works on its own; `--help` lists its commands:

```sh
scripts/yf.py AAPL income --freq quarterly
scripts/screen.py equity --filter "region eq us peratio.lasttwelvemonths lt 15"
scripts/sec.py fetch TSLA --type 10-K --section 1A
scripts/13f.py deltas berkshire --top 25
scripts/fred.py snapshot --group rates
scripts/dcf.py --fcfs 100,120,145 --wacc 9 --net-debt=-500 --shares 50 --current-price 40
```

## Design

- **One tool per task, not per script command.** Every case is offered every plugin tool,
  so related commands share a tool with an enum (`stock_data`'s `data`,
  `institutional_holdings_13f`'s `report`) rather than adding dozens of tools. Script
  commands with no use to the agent are left out: `sec.py cik`, `fred.py info`,
  `screen.py fields`/`values` (the common fields are in `stock_screen`'s description),
  and `13f.py`'s `--csv`/`--html`, which write files.
- **Options use `--name=value`.** The host refuses a whole-element value starting with
  `-` (so the LLM cannot inject options); the `=` form lets negative numbers through, as
  `dcf_model`'s `net_debt` needs.
- **Output.** Tables come back inline, or as a case file when long (`auto`);
  `sec_filing_text` always stores a file, read with `read_file` in parts.
- **Caches** live under `$XDG_CACHE_HOME` (the server passes it): 13F filings in
  `finskills-13f/` (EDGAR archives never change), the S&P 500 list in
  `finance/sp_500.json` (refreshed after 30 days, or with `sp500_pe`'s `refresh`). Nothing
  is written to the plugin directory, which the server watches for reloads.
- **13F roster.** `13f.py` has a built-in watchlist of managers; a JSON file at
  `13f_filings/roster.json` here, or at `FINSKILLS_13F_ROSTER`, replaces it.

### Changes from the original scripts

The scripts and `commands/` came from a Claude Code project; `commands/` keeps the
original slash commands the guides were adapted from. Changes made for the plugin:

- `yt.py` and `/transcript` dropped: identical to `youtube_transcribe`'s.
- `fetch-sp500.py` folded into `sp500-pe.py`, which caches the list instead of writing
  `data/sp_500.json` next to the scripts.
- `screen.py`: a `--filter` may hold several conditions in one string (a tool argument is
  one value); presets and `fields`/`values` updated to the field names and whole-number
  percentages of current yfinance (1.x), which rejected the old ones; dividend yield
  shown as Yahoo gives it (already in percent).
- `fred.py`: series ids may be one comma-separated argument.
- `sec.py`: the User-Agent comes from `SEC_USER_AGENT`, like `13f.py`'s.

Planned: unit tests for the scripts' offline logic (filter parsing, DCF arithmetic) and
a `FINANCE_LIVE_TEST=1` test calling the same checks as `check_config.py`.
