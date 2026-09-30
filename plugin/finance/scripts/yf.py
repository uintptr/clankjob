#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "yfinance>=0.2",
#   "pandas>=2.0",
#   "tabulate>=0.9",
# ]
# ///
"""
yf.py — yfinance CLI wrapper for Claude financial skills.

Usage:
    ./yf.py <ticker> <command> [options]

Commands:
  -- Price & History --
  history           Historical OHLCV prices
  fast-info         Live/cached lightweight quote fields
  info              Full company info dict

  -- Financial Statements --
  income            Income statement  (--freq yearly|quarterly|trailing)
  balance           Balance sheet     (--freq yearly|quarterly|trailing)
  cashflow          Cash flow         (--freq yearly|quarterly|trailing)
  earnings          Earnings          (--freq yearly|quarterly)

  -- Analysis & Estimates --
  recommendations   Analyst buy/hold/sell counts
  upgrades          Upgrades & downgrades
  price-targets     Analyst price targets
  earnings-estimate Earnings estimate by period
  revenue-estimate  Revenue estimate by period
  earnings-dates    Upcoming/past earnings dates
  earnings-history  Historical EPS surprises
  eps-trend         EPS trend over last 90 days
  eps-revisions     EPS revision counts
  growth-estimates  Growth estimates (stock vs sector vs index)
  sustainability    ESG scores

  -- Holdings & Insider --
  major-holders     Major holder breakdown
  institutional     Institutional holders
  mutualfund        Mutual fund holders
  insider-purchases Insider purchase summary
  insider-tx        Insider transactions
  insider-roster    Insider roster

  -- Corporate Actions --
  dividends         Dividend history
  splits            Stock split history
  actions           Combined dividends + splits
  capital-gains     Capital gains (funds)
  shares            Share count history
  shares-full       Full share count history (--start/--end)
  calendar          Earnings & dividend calendar
  sec-filings       Recent SEC filings

  -- Options --
  option-dates      Available option expiry dates
  options           Full option chain for a date (--date YYYY-MM-DD)
  calls             Call options only            (--date YYYY-MM-DD)
  puts              Put options only             (--date YYYY-MM-DD)

  -- News --
  news              Latest news headlines        (--count N, --tab news|all|press)

  -- Metadata --
  isin              ISIN identifier
  history-meta      Historical data metadata
"""

import argparse
import json
import sys
from datetime import datetime

import pandas as pd
import yfinance as yf
from tabulate import tabulate


# ---------------------------------------------------------------------------
# Output helpers
# ---------------------------------------------------------------------------

def _df(df: pd.DataFrame | pd.Series | None, title: str = "") -> None:
    if df is None or (hasattr(df, "empty") and df.empty):
        print(f"[no data for: {title}]")
        return
    if isinstance(df, pd.Series):
        df = df.to_frame(name="value")
    if title:
        print(f"\n=== {title} ===")
    print(tabulate(df, headers="keys", tablefmt="github", floatfmt=".2f"))


def _dict(d: dict | None, title: str = "") -> None:
    if not d:
        print(f"[no data for: {title}]")
        return
    if title:
        print(f"\n=== {title} ===")
    print(json.dumps(d, indent=2, default=str))


def _list(lst: list | None, title: str = "") -> None:
    if not lst:
        print(f"[no data for: {title}]")
        return
    if title:
        print(f"\n=== {title} ===")
    print(json.dumps(lst, indent=2, default=str))


# ---------------------------------------------------------------------------
# Command handlers
# ---------------------------------------------------------------------------

def cmd_history(t: yf.Ticker, args: argparse.Namespace) -> None:
    kwargs: dict = {}
    if args.period:
        kwargs["period"] = args.period
    if args.start:
        kwargs["start"] = args.start
    if args.end:
        kwargs["end"] = args.end
    if args.interval:
        kwargs["interval"] = args.interval
    if not kwargs:
        kwargs["period"] = "1y"
    _df(t.history(**kwargs), "Price History")


def cmd_fast_info(t: yf.Ticker, _args) -> None:
    fi = t.fast_info
    data = {k: getattr(fi, k, None) for k in dir(fi) if not k.startswith("_")}
    _dict(data, "Fast Info")


def cmd_info(t: yf.Ticker, _args) -> None:
    _dict(t.get_info(), "Company Info")


def cmd_income(t: yf.Ticker, args: argparse.Namespace) -> None:
    _df(t.get_income_stmt(freq=args.freq), f"Income Statement ({args.freq})")


def cmd_balance(t: yf.Ticker, args: argparse.Namespace) -> None:
    _df(t.get_balance_sheet(freq=args.freq), f"Balance Sheet ({args.freq})")


def cmd_cashflow(t: yf.Ticker, args: argparse.Namespace) -> None:
    _df(t.get_cash_flow(freq=args.freq), f"Cash Flow ({args.freq})")


def cmd_earnings(t: yf.Ticker, args: argparse.Namespace) -> None:
    freq = args.freq if args.freq in ("yearly", "quarterly") else "yearly"
    _df(t.get_earnings(freq=freq), f"Earnings ({freq})")


def cmd_recommendations(t: yf.Ticker, _args) -> None:
    _df(t.get_recommendations(), "Recommendations")


def cmd_upgrades(t: yf.Ticker, _args) -> None:
    _df(t.get_upgrades_downgrades(), "Upgrades & Downgrades")


def cmd_price_targets(t: yf.Ticker, _args) -> None:
    _dict(t.get_analyst_price_targets(), "Analyst Price Targets")


def cmd_earnings_estimate(t: yf.Ticker, _args) -> None:
    _df(t.get_earnings_estimate(), "Earnings Estimate")


def cmd_revenue_estimate(t: yf.Ticker, _args) -> None:
    _df(t.get_revenue_estimate(), "Revenue Estimate")


def cmd_earnings_dates(t: yf.Ticker, args: argparse.Namespace) -> None:
    _df(t.get_earnings_dates(limit=args.count or 12), "Earnings Dates")


def cmd_earnings_history(t: yf.Ticker, _args) -> None:
    _df(t.get_earnings_history(), "Earnings History (EPS Surprises)")


def cmd_eps_trend(t: yf.Ticker, _args) -> None:
    _df(t.get_eps_trend(), "EPS Trend")


def cmd_eps_revisions(t: yf.Ticker, _args) -> None:
    _df(t.get_eps_revisions(), "EPS Revisions")


def cmd_growth_estimates(t: yf.Ticker, _args) -> None:
    _df(t.get_growth_estimates(), "Growth Estimates")


def cmd_sustainability(t: yf.Ticker, _args) -> None:
    _df(t.get_sustainability(), "Sustainability / ESG")


def cmd_major_holders(t: yf.Ticker, _args) -> None:
    _df(t.get_major_holders(), "Major Holders")


def cmd_institutional(t: yf.Ticker, _args) -> None:
    _df(t.get_institutional_holders(), "Institutional Holders")


def cmd_mutualfund(t: yf.Ticker, _args) -> None:
    _df(t.get_mutualfund_holders(), "Mutual Fund Holders")


def cmd_insider_purchases(t: yf.Ticker, _args) -> None:
    _df(t.get_insider_purchases(), "Insider Purchases")


def cmd_insider_tx(t: yf.Ticker, _args) -> None:
    _df(t.get_insider_transactions(), "Insider Transactions")


def cmd_insider_roster(t: yf.Ticker, _args) -> None:
    _df(t.get_insider_roster_holders(), "Insider Roster")


def cmd_dividends(t: yf.Ticker, _args) -> None:
    _df(t.get_dividends(), "Dividends")


def cmd_splits(t: yf.Ticker, _args) -> None:
    _df(t.get_splits(), "Stock Splits")


def cmd_actions(t: yf.Ticker, _args) -> None:
    _df(t.get_actions(), "Corporate Actions")


def cmd_capital_gains(t: yf.Ticker, _args) -> None:
    _df(t.get_capital_gains(), "Capital Gains")


def cmd_shares(t: yf.Ticker, _args) -> None:
    _df(t.get_shares(), "Share Count")


def cmd_shares_full(t: yf.Ticker, args: argparse.Namespace) -> None:
    _df(t.get_shares_full(start=args.start, end=args.end), "Full Share Count History")


def cmd_calendar(t: yf.Ticker, _args) -> None:
    _dict(t.get_calendar(), "Calendar")


def cmd_sec_filings(t: yf.Ticker, _args) -> None:
    filings = t.get_sec_filings()
    if isinstance(filings, dict):
        _dict(filings, "SEC Filings")
    else:
        _df(filings, "SEC Filings")


def cmd_option_dates(t: yf.Ticker, _args) -> None:
    dates = t.options
    if not dates:
        print("[no option dates available]")
        return
    print("\n=== Option Expiry Dates ===")
    for d in dates:
        print(f"  {d}")


def cmd_options(t: yf.Ticker, args: argparse.Namespace) -> None:
    date = args.date or (t.options[0] if t.options else None)
    if not date:
        print("[no option dates available]")
        return
    chain = t.option_chain(date)
    _df(chain.calls, f"Calls  — expiry {date}")
    _df(chain.puts,  f"Puts   — expiry {date}")


def cmd_calls(t: yf.Ticker, args: argparse.Namespace) -> None:
    date = args.date or (t.options[0] if t.options else None)
    if not date:
        print("[no option dates available]")
        return
    _df(t.option_chain(date).calls, f"Calls — expiry {date}")


def cmd_puts(t: yf.Ticker, args: argparse.Namespace) -> None:
    date = args.date or (t.options[0] if t.options else None)
    if not date:
        print("[no option dates available]")
        return
    _df(t.option_chain(date).puts, f"Puts — expiry {date}")


def cmd_news(t: yf.Ticker, args: argparse.Namespace) -> None:
    tab = args.tab or "news"
    count = args.count or 10
    items = t.get_news(count=count, tab=tab)
    if not items:
        print("[no news]")
        return
    print(f"\n=== News ({tab}) ===")
    for item in items:
        ts = item.get("content", {})
        title = ts.get("title") or item.get("title", "—")
        pub   = ts.get("pubDate") or item.get("providerPublishTime", "")
        url   = ts.get("canonicalUrl", {}).get("url") or ""
        if pub and isinstance(pub, int):
            pub = datetime.fromtimestamp(pub).strftime("%Y-%m-%d %H:%M")
        print(f"  [{pub}] {title}")
        if url:
            print(f"          {url}")


def cmd_isin(t: yf.Ticker, _args) -> None:
    print(f"ISIN: {t.get_isin()}")


def cmd_history_meta(t: yf.Ticker, _args) -> None:
    _dict(t.get_history_metadata(), "History Metadata")


# ---------------------------------------------------------------------------
# Command dispatch table
# ---------------------------------------------------------------------------

COMMANDS: dict[str, tuple] = {
    "history":           (cmd_history,          "Historical OHLCV"),
    "fast-info":         (cmd_fast_info,         "Lightweight live quote"),
    "info":              (cmd_info,              "Full company info"),
    "income":            (cmd_income,            "Income statement"),
    "balance":           (cmd_balance,           "Balance sheet"),
    "cashflow":          (cmd_cashflow,          "Cash flow statement"),
    "earnings":          (cmd_earnings,          "Earnings"),
    "recommendations":   (cmd_recommendations,   "Analyst recommendations"),
    "upgrades":          (cmd_upgrades,          "Upgrades & downgrades"),
    "price-targets":     (cmd_price_targets,     "Analyst price targets"),
    "earnings-estimate": (cmd_earnings_estimate, "Earnings estimate"),
    "revenue-estimate":  (cmd_revenue_estimate,  "Revenue estimate"),
    "earnings-dates":    (cmd_earnings_dates,    "Earnings dates"),
    "earnings-history":  (cmd_earnings_history,  "Historical EPS surprises"),
    "eps-trend":         (cmd_eps_trend,         "EPS trend"),
    "eps-revisions":     (cmd_eps_revisions,     "EPS revisions"),
    "growth-estimates":  (cmd_growth_estimates,  "Growth estimates"),
    "sustainability":    (cmd_sustainability,    "ESG / sustainability"),
    "major-holders":     (cmd_major_holders,     "Major holders"),
    "institutional":     (cmd_institutional,     "Institutional holders"),
    "mutualfund":        (cmd_mutualfund,        "Mutual fund holders"),
    "insider-purchases": (cmd_insider_purchases, "Insider purchases"),
    "insider-tx":        (cmd_insider_tx,        "Insider transactions"),
    "insider-roster":    (cmd_insider_roster,    "Insider roster"),
    "dividends":         (cmd_dividends,         "Dividend history"),
    "splits":            (cmd_splits,            "Stock splits"),
    "actions":           (cmd_actions,           "Corporate actions"),
    "capital-gains":     (cmd_capital_gains,     "Capital gains"),
    "shares":            (cmd_shares,            "Share count"),
    "shares-full":       (cmd_shares_full,       "Full share count history"),
    "calendar":          (cmd_calendar,          "Earnings & dividend calendar"),
    "sec-filings":       (cmd_sec_filings,       "Recent SEC filings"),
    "option-dates":      (cmd_option_dates,      "Available option expiry dates"),
    "options":           (cmd_options,           "Full option chain"),
    "calls":             (cmd_calls,             "Call options"),
    "puts":              (cmd_puts,              "Put options"),
    "news":              (cmd_news,              "Latest news"),
    "isin":              (cmd_isin,              "ISIN identifier"),
    "history-meta":      (cmd_history_meta,      "History metadata"),
}


# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="yf.py",
        description="yfinance CLI — fetch financial data for a ticker",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="Commands:\n" + "\n".join(
            f"  {cmd:<22} {desc}" for cmd, (_, desc) in COMMANDS.items()
        ),
    )
    p.add_argument("ticker",  help="Stock ticker symbol (e.g. AAPL, MSFT, BTC-USD)")
    p.add_argument("command", choices=list(COMMANDS.keys()), help="Data to fetch")

    # History options
    p.add_argument("--period",   default="",   help="Period: 1d 5d 1mo 3mo 6mo 1y 2y 5y 10y ytd max")
    p.add_argument("--start",    default=None, help="Start date YYYY-MM-DD (history/shares-full)")
    p.add_argument("--end",      default=None, help="End date   YYYY-MM-DD (history/shares-full)")
    p.add_argument("--interval", default="1d", help="Interval: 1m 2m 5m 15m 30m 60m 90m 1h 1d 5d 1wk 1mo 3mo")

    # Statement frequency
    p.add_argument("--freq", default="yearly",
                   choices=["yearly", "quarterly", "trailing"],
                   help="Statement frequency (default: yearly)")

    # Options
    p.add_argument("--date",  default=None, help="Option expiry date YYYY-MM-DD")

    # News
    p.add_argument("--count", type=int, default=None, help="Number of items to return")
    p.add_argument("--tab",   default=None, choices=["news", "all", "press"],
                   help="News tab: news | all | press")

    return p


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    parser = build_parser()
    args = parser.parse_args()

    ticker = yf.Ticker(args.ticker.upper())
    handler, _ = COMMANDS[args.command]

    try:
        handler(ticker, args)
    except Exception as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
