#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "httpx>=0.27",
#   "tabulate>=0.9",
# ]
# ///
"""
fred.py — FRED (Federal Reserve Economic Data) CLI.

Fetches macro economic indicators from the St. Louis Fed API.

Usage:
    ./fred.py <command> [options]

API key:
    Set env var FRED_API_KEY or pass --api-key.
    Get a free key at https://fred.stlouisfed.org/docs/api/api_key.html

Commands:
  series      Fetch observations for one or more series IDs
  search      Search for series by keyword
  info        Metadata for a series (title, frequency, units, etc.)
  snapshot    Pre-built macro dashboard (key series in one shot)

Preset groups (use with --group):
  rates       Fed Funds, 2yr, 10yr, 30yr, yield spread, real rate
  inflation   CPI, Core CPI, PCE, Core PCE, breakeven inflation
  growth      Real GDP, GDP growth, Industrial Production, Retail Sales
  labour      Unemployment (U3, U6), Non-farm payrolls, JOLTS
  credit      HY spread, IG spread, TED spread, 30yr mortgage rate
  housing     Case-Shiller, Housing starts, Existing home sales
  money       M2, Fed balance sheet (WALCL)
  sentiment   Consumer sentiment (Michigan), Conference Board LEI

Example:
    ./fred.py series FEDFUNDS DGS10 --count 12
    ./fred.py snapshot
    ./fred.py snapshot --group inflation
    ./fred.py search "yield curve"
    ./fred.py info UNRATE
"""

import argparse
import json
import os
import re
import sys
from datetime import date, timedelta

import httpx
from tabulate import tabulate

BASE_URL = "https://api.stlouisfed.org/fred"

# ---------------------------------------------------------------------------
# Well-known series
# ---------------------------------------------------------------------------

SERIES_GROUPS: dict[str, list[tuple[str, str]]] = {
    "rates": [
        ("FEDFUNDS",    "Fed Funds Rate (%)"),
        ("DGS2",        "2yr Treasury Yield (%)"),
        ("DGS10",       "10yr Treasury Yield (%)"),
        ("DGS30",       "30yr Treasury Yield (%)"),
        ("T10Y2Y",      "10yr-2yr Spread (%)"),
        ("DFII10",      "10yr Real Yield TIPS (%)"),
    ],
    "inflation": [
        ("CPIAUCSL",    "CPI (All Urban, SA, YoY%)"),
        ("CPILFESL",    "Core CPI excl. Food & Energy (%)"),
        ("PCEPI",       "PCE Price Index"),
        ("PCEPILFE",    "Core PCE (Fed target series)"),
        ("T10YIE",      "10yr Breakeven Inflation (%)"),
    ],
    "growth": [
        ("GDPC1",       "Real GDP (Bil. 2017 USD, SAAR)"),
        ("A191RL1Q225SBEA", "Real GDP Growth QoQ (%)"),
        ("INDPRO",      "Industrial Production Index"),
        ("RSXFS",       "Retail Sales excl. Food Svcs"),
        ("BOPTEXP",     "Exports of Goods & Services"),
    ],
    "labour": [
        ("UNRATE",      "Unemployment Rate U-3 (%)"),
        ("U6RATE",      "Underemployment Rate U-6 (%)"),
        ("PAYEMS",      "Nonfarm Payrolls (Thousands)"),
        ("JTSJOL",      "Job Openings (JOLTS, Thousands)"),
        ("ICSA",        "Initial Jobless Claims (Weekly)"),
    ],
    "credit": [
        ("BAMLH0A0HYM2", "US HY Option-Adj Spread (%)"),
        ("BAMLC0A0CM",   "US IG Option-Adj Spread (%)"),
        ("TEDRATE",      "TED Spread (%)"),
        ("MORTGAGE30US", "30yr Fixed Mortgage Rate (%)"),
    ],
    "housing": [
        ("CSUSHPISA",   "Case-Shiller US Home Price Index"),
        ("HOUST",       "Housing Starts (Thousands SAAR)"),
        ("EXHOSLUSM495S","Existing Home Sales (Millions)"),
        ("MSPUS",       "Median Sales Price of Houses ($)"),
    ],
    "money": [
        ("M2SL",        "M2 Money Supply (Bil. USD)"),
        ("WALCL",       "Fed Balance Sheet Total Assets (Mil. USD)"),
        ("BOGMBASE",    "Monetary Base (Bil. USD)"),
    ],
    "sentiment": [
        ("UMCSENT",     "U of Michigan Consumer Sentiment"),
        ("USSLIND",     "Conference Board LEI"),
        ("STLFSI4",     "St. Louis Financial Stress Index"),
    ],
}

ALL_SNAPSHOT = [s for group in SERIES_GROUPS.values() for s in group]


# ---------------------------------------------------------------------------
# API helpers
# ---------------------------------------------------------------------------

def _series_ids(given: list[str]) -> list[str]:
    """Series ids from the command line, each argument possibly a comma or space separated list."""
    return [sid for arg in given for sid in re.split(r"[,\s]+", arg.upper()) if sid]


def _api_key(args: argparse.Namespace) -> str:
    key = args.api_key or os.environ.get("FRED_API_KEY", "")
    if not key:
        print(
            "Error: FRED API key required.\n"
            "  Set env var FRED_API_KEY or pass --api-key.\n"
            "  Free key: https://fred.stlouisfed.org/docs/api/api_key.html",
            file=sys.stderr,
        )
        sys.exit(1)
    return key


def _get(path: str, params: dict) -> dict:
    params["file_type"] = "json"
    try:
        r = httpx.get(f"{BASE_URL}/{path}", params=params, timeout=15)
        r.raise_for_status()
        return r.json()
    except httpx.HTTPStatusError as e:
        print(f"HTTP error {e.response.status_code}: {e.response.text}", file=sys.stderr)
        sys.exit(1)
    except httpx.RequestError as e:
        print(f"Request error: {e}", file=sys.stderr)
        sys.exit(1)


def _latest_value(series_id: str, api_key: str) -> tuple[str, str]:
    """Return (date, value) of most recent observation."""
    data = _get("series/observations", {
        "series_id": series_id,
        "api_key": api_key,
        "sort_order": "desc",
        "limit": 1,
    })
    obs = data.get("observations", [])
    if not obs:
        return ("—", "—")
    o = obs[0]
    return (o.get("date", "—"), o.get("value", "—"))


def _observations(series_id: str, api_key: str, count: int,
                  start: str | None, end: str | None,
                  freq: str | None) -> list[dict]:
    params: dict = {
        "series_id": series_id,
        "api_key": api_key,
        "sort_order": "desc",
        "limit": count,
    }
    if start:
        params["observation_start"] = start
    if end:
        params["observation_end"] = end
    if freq:
        params["frequency"] = freq
        params["aggregation_method"] = "eop"  # end-of-period
    data = _get("series/observations", params)
    obs = data.get("observations", [])
    return list(reversed(obs))  # chronological order


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def cmd_series(args: argparse.Namespace) -> None:
    key = _api_key(args)
    for sid in _series_ids(args.series_ids):
        obs = _observations(sid, key, args.count, args.start, args.end, args.freq)
        if not obs:
            print(f"\n[no data for {sid}]")
            continue
        rows = [(o["date"], o["value"]) for o in obs]
        print(f"\n=== {sid} ===")
        print(tabulate(rows, headers=["Date", "Value"], tablefmt="github"))


def cmd_info(args: argparse.Namespace) -> None:
    key = _api_key(args)
    for sid in _series_ids(args.series_ids):
        data = _get("series", {"series_id": sid, "api_key": key})
        series_list = data.get("seriess", [])
        if not series_list:
            print(f"\n[no info for {sid}]")
            continue
        s = series_list[0]
        print(f"\n=== {sid} — {s.get('title', '')} ===")
        fields = [
            ("ID",                  s.get("id")),
            ("Title",               s.get("title")),
            ("Frequency",           s.get("frequency")),
            ("Units",               s.get("units")),
            ("Seasonal Adjustment", s.get("seasonal_adjustment")),
            ("Observation Start",   s.get("observation_start")),
            ("Observation End",     s.get("observation_end")),
            ("Last Updated",        s.get("last_updated")),
            ("Popularity",          s.get("popularity")),
        ]
        print(tabulate(fields, tablefmt="github"))
        if s.get("notes"):
            print(f"\nNotes: {s['notes'][:400]}")


def cmd_search(args: argparse.Namespace) -> None:
    key = _api_key(args)
    params = {
        "search_text": args.query,
        "api_key": key,
        "limit": args.limit,
        "search_type": "full_text",
    }
    if args.filter_freq:
        params["filter_variable"] = "frequency"
        params["filter_value"] = args.filter_freq
    data = _get("series/search", params)
    results = data.get("seriess", [])
    if not results:
        print("[no results]")
        return
    rows = [
        (s["id"], s["title"][:60], s["frequency_short"], s["units_short"], s["observation_end"])
        for s in results
    ]
    print(f"\n=== Search: '{args.query}' ({len(rows)} results) ===")
    print(tabulate(rows, headers=["ID", "Title", "Freq", "Units", "Last Obs"], tablefmt="github"))


def cmd_snapshot(args: argparse.Namespace) -> None:
    key = _api_key(args)

    if args.group:
        if args.group not in SERIES_GROUPS:
            print(f"Unknown group '{args.group}'. Available: {', '.join(SERIES_GROUPS)}", file=sys.stderr)
            sys.exit(1)
        series_list = SERIES_GROUPS[args.group]
        title = f"Macro Snapshot — {args.group.title()}"
    else:
        series_list = ALL_SNAPSHOT
        title = "Macro Snapshot — All"

    print(f"\n=== {title} ===")
    print(f"Data source: FRED (St. Louis Fed) · as of {date.today()}\n")

    rows = []
    for sid, label in series_list:
        dt, val = _latest_value(sid, key)
        rows.append((sid, label, val, dt))

    print(tabulate(rows, headers=["Series ID", "Indicator", "Latest Value", "Date"], tablefmt="github"))

    print("\nGroups available via --group:")
    for g in SERIES_GROUPS:
        print(f"  {g:<12} ({len(SERIES_GROUPS[g])} series)")


# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="fred.py",
        description="FRED macro data CLI",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument("--api-key", default=None,
                   help="FRED API key (or set FRED_API_KEY env var)")

    sub = p.add_subparsers(dest="command", required=True)

    # series
    s = sub.add_parser("series", help="Fetch observations for series IDs")
    s.add_argument("series_ids", nargs="+", metavar="SERIES_ID")
    s.add_argument("--count",  type=int, default=12, help="Number of observations (default 12)")
    s.add_argument("--start",  default=None, help="Start date YYYY-MM-DD")
    s.add_argument("--end",    default=None, help="End date YYYY-MM-DD")
    s.add_argument("--freq",   default=None,
                   choices=["d","w","bw","m","q","sa","a"],
                   help="Aggregate to frequency: d w bw m q sa a")

    # info
    i = sub.add_parser("info", help="Series metadata")
    i.add_argument("series_ids", nargs="+", metavar="SERIES_ID")

    # search
    sr = sub.add_parser("search", help="Search for series by keyword")
    sr.add_argument("query", help="Search text")
    sr.add_argument("--limit", type=int, default=20, help="Max results (default 20)")
    sr.add_argument("--filter-freq", default=None,
                    choices=["Daily","Weekly","Monthly","Quarterly","Annual"],
                    help="Filter by frequency")

    # snapshot
    sn = sub.add_parser("snapshot", help="Pre-built macro dashboard")
    sn.add_argument("--group", default=None,
                    choices=list(SERIES_GROUPS.keys()),
                    help="Show only one group of indicators")

    return p


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    parser = build_parser()
    args = parser.parse_args()

    dispatch = {
        "series":   cmd_series,
        "info":     cmd_info,
        "search":   cmd_search,
        "snapshot": cmd_snapshot,
    }
    try:
        dispatch[args.command](args)
    except Exception as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
