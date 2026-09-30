#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "yfinance>=0.2",
#   "pandas>=2.0",
#   "lxml>=4.9",
#   "tabulate>=0.9",
# ]
# ///
"""
sp500-pe.py — S&P 500 constituents ranked by trailing P/E ratio.

Downloads the constituent list from Wikipedia and caches it for 30 days in
$XDG_CACHE_HOME/finance/sp_500.json (default ~/.cache), then batch-downloads
valuation data from Yahoo Finance.

Usage:
    ./scripts/sp500-pe.py                        # all 500, sorted by P/E asc
    ./scripts/sp500-pe.py --count 50             # top 50 lowest P/E
    ./scripts/sp500-pe.py --sort pe-desc         # highest P/E first
    ./scripts/sp500-pe.py --sort market-cap      # by market cap desc
    ./scripts/sp500-pe.py --sector Technology    # filter by sector
    ./scripts/sp500-pe.py --include-negative     # include loss-making companies
    ./scripts/sp500-pe.py --refresh              # download the constituent list again
"""

import argparse
import io
import json
import os
import sys
import time
import urllib.request
from datetime import date
from pathlib import Path

import pandas as pd
import yfinance as yf
from tabulate import tabulate

WIKIPEDIA_URL = "https://en.wikipedia.org/wiki/List_of_S%26P_500_companies"
HEADERS = {"User-Agent": "Mozilla/5.0 (compatible; sp500-fetch/1.0)"}
CACHE_DIR = Path(os.environ.get("XDG_CACHE_HOME") or Path.home() / ".cache") / "finance"
SP500_JSON = CACHE_DIR / "sp_500.json"
# The index changes a few times a year.
MAX_AGE_SECONDS = 30 * 86400


def fetch_constituents() -> list[dict]:
    """Download the constituent list from Wikipedia: {ticker, name, sector}, sorted by ticker."""
    print("Fetching S&P 500 constituents from Wikipedia...", file=sys.stderr)
    request = urllib.request.Request(WIKIPEDIA_URL, headers=HEADERS)
    with urllib.request.urlopen(request, timeout=30) as response:
        html = response.read().decode("utf-8")

    df = pd.read_html(io.StringIO(html))[0]
    df.columns = [c.strip() for c in df.columns]

    ticker_col = next(c for c in df.columns if "symbol" in c.lower())
    name_col   = next((c for c in df.columns if "security" in c.lower()), None)
    sector_col = next((c for c in df.columns if "gics sector" in c.lower()), None)

    records = []
    for _, row in df.iterrows():
        ticker = str(row[ticker_col]).replace(".", "-")  # BRK.B → BRK-B
        record = {"ticker": ticker}
        if name_col:
            record["name"] = str(row[name_col])
        if sector_col:
            record["sector"] = str(row[sector_col])
        records.append(record)

    records.sort(key=lambda r: r["ticker"])
    return records


def load_constituents(refresh: bool) -> pd.DataFrame:
    fresh = SP500_JSON.exists() and time.time() - SP500_JSON.stat().st_mtime < MAX_AGE_SECONDS
    if refresh or not fresh:
        try:
            records = fetch_constituents()
        except (OSError, ValueError, StopIteration) as exc:
            if not SP500_JSON.exists():
                print(f"Error: cannot download the S&P 500 list: {exc}", file=sys.stderr)
                sys.exit(1)
            print(f"warning: using the cached list, the download failed: {exc}", file=sys.stderr)
        else:
            CACHE_DIR.mkdir(parents=True, exist_ok=True)
            SP500_JSON.write_text(json.dumps(records, indent=2))
            print(f"Saved {len(records)} constituents to {SP500_JSON}", file=sys.stderr)
            return pd.DataFrame(records)
    return pd.DataFrame(json.loads(SP500_JSON.read_text()))


def fetch_valuation(tickers: list[str]) -> pd.DataFrame:
    """Batch-download valuation fields via yfinance."""
    print(f"Downloading valuation data for {len(tickers)} tickers (this may take ~2min)...", file=sys.stderr)
    t_obj = yf.Tickers(" ".join(tickers))

    rows = []
    total = len(tickers)
    for i, ticker in enumerate(tickers, 1):
        if i % 50 == 0 or i == total:
            print(f"  Processed {i}/{total}...", file=sys.stderr)
        try:
            full = t_obj.tickers[ticker].info
            rows.append({
                "ticker":      ticker,
                "price":       full.get("currentPrice") or full.get("regularMarketPrice"),
                "trailingPE":  full.get("trailingPE"),
                "forwardPE":   full.get("forwardPE"),
                "marketCap":   full.get("marketCap"),
                "sector":      full.get("sector"),
                "industry":    full.get("industry"),
                "roe":         full.get("returnOnEquity"),
                "grossMargin": full.get("grossMargins"),
                "divYield":    full.get("dividendYield"),
            })
        except Exception:
            rows.append({"ticker": ticker})

    return pd.DataFrame(rows)


def fmt_cap(v):
    if pd.isna(v) or v is None:
        return "—"
    if v >= 1e12:
        return f"${v/1e12:.1f}T"
    if v >= 1e9:
        return f"${v/1e9:.1f}B"
    return f"${v/1e6:.0f}M"


def fmt_pct(v):
    """Format a decimal (e.g. 0.21) as a percentage string."""
    if pd.isna(v) or v is None:
        return "—"
    return f"{v*100:.1f}%"


def fmt_yield(v):
    """Format dividendYield — yfinance returns it already as a percentage (e.g. 4.28)."""
    if pd.isna(v) or v is None:
        return "—"
    return f"{v:.2f}%"


def fmt_pe(v):
    if pd.isna(v) or v is None:
        return "—"
    return f"{v:.1f}x"


def main():
    parser = argparse.ArgumentParser(
        description="S&P 500 constituents ranked by P/E ratio",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("--count", type=int, default=500, help="Max rows to show (default: all)")
    parser.add_argument(
        "--sort",
        choices=["pe-asc", "pe-desc", "forward-pe", "market-cap"],
        default="pe-asc",
        help="Sort order (default: pe-asc = lowest trailing P/E first)",
    )
    parser.add_argument("--sector", default=None, help="Filter to a specific GICS sector")
    parser.add_argument(
        "--include-negative",
        action="store_true",
        help="Include companies with negative P/E (loss-making)",
    )
    parser.add_argument(
        "--refresh",
        action="store_true",
        help="Download the constituent list again, even if the cached one is recent",
    )
    args = parser.parse_args()

    # 1. Load the constituent list (cached)
    constituents = load_constituents(args.refresh)
    tickers = constituents["ticker"].tolist()

    # 2. Fetch live valuation data
    val = fetch_valuation(tickers)

    # 3. Merge: prefer sector from yfinance, fall back to JSON
    df = constituents.merge(
        val[["ticker", "price", "trailingPE", "forwardPE",
             "marketCap", "sector", "industry", "roe", "grossMargin", "divYield"]],
        on="ticker", how="left",
    )
    if "sector_x" in df.columns and "sector_y" in df.columns:
        df["sector"] = df["sector_y"].fillna(df["sector_x"])
        df.drop(columns=["sector_x", "sector_y"], inplace=True)

    # 4. Filter
    if args.sector:
        df = df[df["sector"].str.lower().str.contains(args.sector.lower(), na=False)]
        if df.empty:
            print(f"No results for sector '{args.sector}'")
            sys.exit(0)

    if not args.include_negative:
        df = df[df["trailingPE"].isna() | (df["trailingPE"] > 0)]

    # 5. Sort
    sort_map = {
        "pe-asc":     ("trailingPE", True),
        "pe-desc":    ("trailingPE", False),
        "forward-pe": ("forwardPE",  True),
        "market-cap": ("marketCap",  False),
    }
    sort_col, sort_asc = sort_map[args.sort]
    df = df.sort_values(sort_col, ascending=sort_asc, na_position="last")

    # 6. Format and display
    display = df.head(args.count).copy()
    display["trailingPE"]  = display["trailingPE"].apply(fmt_pe)
    display["forwardPE"]   = display["forwardPE"].apply(fmt_pe)
    display["marketCap"]   = display["marketCap"].apply(fmt_cap)
    display["roe"]         = display["roe"].apply(fmt_pct)
    display["grossMargin"] = display["grossMargin"].apply(fmt_pct)
    display["divYield"]    = display["divYield"].apply(fmt_yield)
    display["price"]       = display["price"].apply(lambda v: f"${v:.2f}" if pd.notna(v) else "—")

    display = display.rename(columns={
        "ticker":      "Ticker",
        "name":        "Name",
        "price":       "Price",
        "trailingPE":  "P/E (TTM)",
        "forwardPE":   "Fwd P/E",
        "marketCap":   "Mkt Cap",
        "sector":      "Sector",
        "industry":    "Industry",
        "roe":         "ROE",
        "grossMargin": "Gross Mgn",
        "divYield":    "Div Yield",
    })

    cols = ["Ticker", "Name", "Price", "P/E (TTM)", "Fwd P/E", "Mkt Cap",
            "Sector", "ROE", "Gross Mgn", "Div Yield"]
    available = [c for c in cols if c in display.columns]
    display = display[available].fillna("—")

    sort_label = {
        "pe-asc":     "Trailing P/E ascending (cheapest first)",
        "pe-desc":    "Trailing P/E descending (most expensive first)",
        "forward-pe": "Forward P/E ascending",
        "market-cap": "Market Cap descending",
    }[args.sort]

    sector_label = f" | Sector: {args.sector}" if args.sector else ""
    neg_label    = " | including negative P/E" if args.include_negative else ""
    print(f"\n=== S&P 500 — {sort_label}{sector_label}{neg_label} ({len(display)} shown) ===")
    print(f"Data: Yahoo Finance via yfinance | Constituent list: Wikipedia | As of: {date.today()}\n")
    print(tabulate(display, headers="keys", tablefmt="github", showindex=False))
    print("\nDisclaimer: For informational purposes only. Not investment advice. Verify independently.")


if __name__ == "__main__":
    main()
