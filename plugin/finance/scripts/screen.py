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
screen.py — Stock screener using yfinance EquityQuery / FundQuery.

Wraps yfinance.screen() with convenient presets and a flexible filter CLI.

Usage:
    ./screen.py <command> [options]

Commands:
  equity      Screen equities with custom filters
  fund        Screen mutual funds / ETFs
  preset      Run a named preset screen
  fields      List valid EquityQuery field names
  values      List valid categorical values for a field

Presets (use with: ./screen.py preset <name>):
  value           Low P/E + P/B, US large-cap
  growth          High revenue growth + strong margins
  dividend        High yield + payout sustainability
  momentum        Strong 52-week price performance
  quality         High ROE + low debt, profitable
  small-cap-value Small cap with low P/B
  tech-growth     Tech sector, high growth estimates
  deep-value      Graham-style: P/E<15, P/B<1.5

Example filters (--filter KEY OP VALUE):
  --filter peratio.lasttwelvemonths lt 15
  --filter intradaymarketcap gt 10000000000
  --filter sector eq Technology
  --filter region eq us
  --filter percentchange gt 5
  --filter pricebookratio.quarterly lt 3
  --filter returnonequity.lasttwelvemonths gt 15   (percentages as whole numbers)
  --filter "region eq us sector eq 'Consumer Cyclical'"   (several in one string)

Operators: gt, lt, gte, lte, eq, btwn (use btwn with two values: KEY btwn LOW HIGH)

Examples:
    ./screen.py preset value
    ./screen.py preset dividend --count 20
    ./screen.py equity --filter sector eq Technology --filter peratio.lasttwelvemonths lt 25 --filter intradaymarketcap gt 5000000000
    ./screen.py equity --filter region eq us --filter percentchange gt 10 --sort-by percentchange
    ./screen.py fund --filter categoryname eq "Large Growth" --filter performanceratingoverall gte 4
    ./screen.py fields
    ./screen.py values sector
"""

import argparse
import shlex
import sys

import pandas as pd
import yfinance as yf
from tabulate import tabulate

# ---------------------------------------------------------------------------
# Preset definitions
# ---------------------------------------------------------------------------

def _eq(*args):
    return yf.EquityQuery(*args)


# Yahoo's screener takes percentages as whole numbers (15 = 15%).
PRESETS: dict[str, dict] = {
    "value": {
        "description": "US large-cap value: low P/E, low P/B, profitable",
        "query": lambda: _eq("and", [
            _eq("eq",  ["region", "us"]),
            _eq("lt",  ["peratio.lasttwelvemonths", 15]),
            _eq("lt",  ["pricebookratio.quarterly", 2]),
            _eq("gt",  ["intradaymarketcap", 2_000_000_000]),
            _eq("gt",  ["netincomeis.lasttwelvemonths", 0]),
        ]),
        "sort_by": "peratio.lasttwelvemonths",
    },
    "growth": {
        "description": "High revenue growth with strong margins",
        "query": lambda: _eq("and", [
            _eq("eq",  ["region", "us"]),
            _eq("gt",  ["totalrevenues1yrgrowth.lasttwelvemonths", 15]),
            _eq("gt",  ["ebitdamargin.lasttwelvemonths", 10]),
            _eq("gt",  ["intradaymarketcap", 500_000_000]),
        ]),
        "sort_by": "totalrevenues1yrgrowth.lasttwelvemonths",
    },
    "dividend": {
        "description": "High dividend yield, profitable",
        "query": lambda: _eq("and", [
            _eq("eq",  ["region", "us"]),
            _eq("gt",  ["dividendyield", 3]),
            _eq("gt",  ["intradaymarketcap", 1_000_000_000]),
            _eq("gt",  ["netincomeis.lasttwelvemonths", 0]),
        ]),
        "sort_by": "dividendyield",
    },
    "momentum": {
        "description": "Strong 52-week price performance",
        "query": lambda: _eq("and", [
            _eq("eq",  ["region", "us"]),
            _eq("gt",  ["fiftytwowkpercentchange", 20]),
            _eq("gt",  ["avgdailyvol3m", 500_000]),
            _eq("gt",  ["intradaymarketcap", 300_000_000]),
        ]),
        "sort_by": "fiftytwowkpercentchange",
    },
    "quality": {
        "description": "High ROE, low debt, consistent profitability",
        "query": lambda: _eq("and", [
            _eq("eq",  ["region", "us"]),
            _eq("gt",  ["returnonequity.lasttwelvemonths", 15]),
            _eq("btwn", ["totaldebtequity.lasttwelvemonths", 0, 100]),
            _eq("gt",  ["grossprofitmargin.lasttwelvemonths", 30]),
            _eq("gt",  ["intradaymarketcap", 1_000_000_000]),
        ]),
        "sort_by": "returnonequity.lasttwelvemonths",
    },
    "small-cap-value": {
        "description": "Small-cap stocks with low valuation multiples",
        "query": lambda: _eq("and", [
            _eq("eq",  ["region", "us"]),
            _eq("btwn", ["intradaymarketcap", 300_000_000, 2_000_000_000]),
            _eq("lt",  ["pricebookratio.quarterly", 1.5]),
            _eq("lt",  ["peratio.lasttwelvemonths", 15]),
            _eq("gt",  ["netincomeis.lasttwelvemonths", 0]),
        ]),
        "sort_by": "pricebookratio.quarterly",
    },
    "tech-growth": {
        "description": "Technology sector with high EPS growth",
        "query": lambda: _eq("and", [
            _eq("eq",  ["sector", "Technology"]),
            _eq("eq",  ["region", "us"]),
            _eq("gt",  ["epsgrowth.lasttwelvemonths", 10]),
            _eq("gt",  ["intradaymarketcap", 1_000_000_000]),
        ]),
        "sort_by": "epsgrowth.lasttwelvemonths",
    },
    "deep-value": {
        "description": "Graham-style deep value: P/E<15, P/B<1.5, earnings positive",
        "query": lambda: _eq("and", [
            _eq("eq",  ["region", "us"]),
            _eq("lt",  ["peratio.lasttwelvemonths", 15]),
            _eq("lt",  ["pricebookratio.quarterly", 1.5]),
            _eq("gt",  ["currentratio.lasttwelvemonths", 1.5]),
            _eq("gt",  ["netincomeis.lasttwelvemonths", 0]),
        ]),
        "sort_by": "peratio.lasttwelvemonths",
    },
}


# Output columns to show for equity results
EQUITY_COLUMNS = [
    "symbol", "shortName", "sector", "industry",
    "currentPrice", "marketCap",
    "trailingPE", "priceToBook",
    "returnOnEquity", "grossMargins",
    "revenueGrowth", "dividendYield",
]

FUND_COLUMNS = [
    "symbol", "shortName", "category",
    "totalAssets", "ytdReturn",
    "threeYearAverageReturn", "fiveYearAverageReturn",
    "expenseRatio",
]


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _run_screen(query, count: int, sort_by: str | None = None) -> pd.DataFrame:
    """Execute yfinance.screen() and return a DataFrame."""
    kwargs: dict = {"query": query, "size": count}
    if sort_by:
        kwargs["sortField"] = sort_by
        kwargs["sortAsc"] = False
    try:
        result = yf.screen(**kwargs)
    except Exception as e:
        print(f"Screen error: {e}", file=sys.stderr)
        sys.exit(1)

    quotes = result.get("quotes", []) if isinstance(result, dict) else result
    if not quotes:
        return pd.DataFrame()
    return pd.DataFrame(quotes)


def _display(df: pd.DataFrame, cols: list[str], title: str) -> None:
    if df.empty:
        print("[no results]")
        return

    # Keep only columns that actually exist
    available = [c for c in cols if c in df.columns]
    subset = df[available].copy()

    # Format marketCap
    if "marketCap" in subset.columns:
        subset["marketCap"] = subset["marketCap"].apply(
            lambda v: f"${v/1e9:.1f}B" if pd.notna(v) and v >= 1e9
            else (f"${v/1e6:.0f}M" if pd.notna(v) else "—")
        )

    # Format percentages: fractions, except dividendYield, which Yahoo gives in percent
    pct_cols = ["returnOnEquity", "grossMargins", "revenueGrowth",
                "ytdReturn", "threeYearAverageReturn",
                "fiveYearAverageReturn", "expenseRatio"]
    for col in pct_cols:
        if col in subset.columns:
            subset[col] = subset[col].apply(
                lambda v: f"{v*100:.1f}%" if pd.notna(v) else "—"
            )
    if "dividendYield" in subset.columns:
        subset["dividendYield"] = subset["dividendYield"].apply(
            lambda v: f"{v:.2f}%" if pd.notna(v) else "—"
        )

    # Format totalAssets for funds
    if "totalAssets" in subset.columns:
        subset["totalAssets"] = subset["totalAssets"].apply(
            lambda v: f"${v/1e9:.1f}B" if pd.notna(v) and v >= 1e9
            else (f"${v/1e6:.0f}M" if pd.notna(v) else "—")
        )

    subset = subset.fillna("—")
    print(f"\n=== {title} ({len(df)} results) ===")
    print(tabulate(subset, headers="keys", tablefmt="github", showindex=False))


def _parse_filter(filter_args: list[str]) -> "yf.EquityQuery | None":
    """
    Parse --filter KEY OP VALUE [VALUE2] triplets into nested EquityQuery.
    """
    if not filter_args:
        return None

    # Tokenise: split on whitespace, but filters arrive as individual tokens already
    # Because argparse nargs='+', each --filter gets a list of tokens
    conditions = []
    i = 0
    tokens = filter_args

    while i < len(tokens):
        key = tokens[i]
        if i + 1 >= len(tokens):
            print(f"Error: --filter needs KEY OP VALUE (got '{key}')", file=sys.stderr)
            sys.exit(1)
        op  = tokens[i + 1].lower()
        if i + 2 >= len(tokens):
            print(f"Error: --filter needs KEY OP VALUE (got '{key} {op}')", file=sys.stderr)
            sys.exit(1)
        val = tokens[i + 2]
        i += 3

        # Try to coerce to number
        def _coerce(v):
            try:
                return int(v)
            except ValueError:
                try:
                    return float(v)
                except ValueError:
                    return v

        if op == "btwn":
            if i >= len(tokens):
                print("Error: btwn requires two values: KEY btwn LOW HIGH", file=sys.stderr)
                sys.exit(1)
            val2 = tokens[i]
            i += 1
            conditions.append(_eq("btwn", [key, _coerce(val), _coerce(val2)]))
        elif op == "is-in":
            # Consume remaining tokens as values until next key (heuristic: stop at known operators)
            vals = [_coerce(val)]
            while i < len(tokens) and tokens[i].lower() not in ("gt","lt","gte","lte","eq","btwn","is-in"):
                vals.append(_coerce(tokens[i]))
                i += 1
            conditions.append(_eq("is-in", [key] + vals))
        else:
            conditions.append(_eq(op, [key, _coerce(val)]))

    if len(conditions) == 1:
        return conditions[0]
    return _eq("and", conditions)


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def _filter_tokens(groups: list[list[str]]) -> list[str]:
    """Flatten --filter lists into one token list. A filter may also come as one string,
    split like a shell line: --filter "pe lt 15 sector eq 'Consumer Cyclical'"."""
    return [tok for group in groups for tok in (shlex.split(group[0]) if len(group) == 1 else group)]


def cmd_equity(args: argparse.Namespace) -> None:
    if not args.filter:
        print("Error: provide at least one --filter KEY OP VALUE", file=sys.stderr)
        sys.exit(1)

    tokens = _filter_tokens(args.filter)
    query = _parse_filter(tokens)

    df = _run_screen(query, args.count, args.sort_by)
    cols = args.columns.split(",") if args.columns else EQUITY_COLUMNS
    _display(df, cols, "Equity Screen Results")


def cmd_fund(args: argparse.Namespace) -> None:
    if not args.filter:
        print("Error: provide at least one --filter KEY OP VALUE", file=sys.stderr)
        sys.exit(1)

    tokens = _filter_tokens(args.filter)

    # Build FundQuery
    conditions = []
    i = 0
    while i < len(tokens):
        key = tokens[i]
        op  = tokens[i+1].lower() if i+1 < len(tokens) else ""
        val = tokens[i+2] if i+2 < len(tokens) else ""
        i += 3
        def _c(v):
            try: return int(v)
            except: pass
            try: return float(v)
            except: return v
        if op == "is-in":
            vals = [_c(val)]
            while i < len(tokens) and tokens[i].lower() not in ("gt","lt","gte","lte","eq","btwn","is-in"):
                vals.append(_c(tokens[i]))
                i += 1
            conditions.append(yf.FundQuery("is-in", [key] + vals))
        else:
            conditions.append(yf.FundQuery(op, [key, _c(val)]))

    if len(conditions) == 1:
        query = conditions[0]
    else:
        query = yf.FundQuery("and", conditions)

    df = _run_screen(query, args.count, args.sort_by)
    _display(df, FUND_COLUMNS, "Fund Screen Results")


def cmd_preset(args: argparse.Namespace) -> None:
    name = args.name.lower()
    if name not in PRESETS:
        print(f"Unknown preset '{name}'. Available: {', '.join(PRESETS)}", file=sys.stderr)
        sys.exit(1)

    p = PRESETS[name]
    print(f"Preset: {name} — {p['description']}")
    query = p["query"]()
    sort_by = args.sort_by or p.get("sort_by")
    df = _run_screen(query, args.count, sort_by)
    _display(df, EQUITY_COLUMNS, f"Preset: {name}")


def _any_equity_query() -> "yf.EquityQuery":
    """A query instance: recent yfinance only exposes valid_fields / valid_values on instances."""
    return yf.EquityQuery("gt", ["intradaymarketcap", 0])


def cmd_fields(args: argparse.Namespace) -> None:
    print("\n=== EquityQuery Valid Fields ===")
    try:
        fields = _any_equity_query().valid_fields
        if isinstance(fields, dict):
            for category, field_list in fields.items():
                print(f"\n{category}:")
                for f in field_list:
                    print(f"  {f}")
        else:
            for f in sorted(fields):
                print(f"  {f}")
    except AttributeError:
        print("(valid_fields not available in this yfinance version)")
        print("Known fields include: pe, pricebookratio.quarterly, peratio.lasttwelvemonths,")
        print("  marketcap, intradaymarketcap, sector, region, exchange,")
        print("  percentchange, fiftytwowkpercentchange, avgdailyvol3m,")
        print("  returnonequity.lasttwelvemonths, grossmargins.lasttwelvemonths,")
        print("  ebitdamargins.lasttwelvemonths, revenuegrowth.lasttwelvemonths,")
        print("  netincometocommon.lasttwelvemonths, debttotalequity.quarterly,")
        print("  currentratio.quarterly, dividendyield, payoutratio, epsforwardgrowth,")
        print("  beta, dayvolume, eodvolume, pctheldinsider, pctheldinst")


def cmd_values(args: argparse.Namespace) -> None:
    print(f"\n=== EquityQuery Valid Values for: {args.field} ===")
    try:
        vals = _any_equity_query().valid_values
        if isinstance(vals, dict) and args.field in vals:
            for v in vals[args.field]:
                print(f"  {v}")
        else:
            print(f"(no restricted value set for '{args.field}' — accepts numeric values)")
    except AttributeError:
        print("(valid_values not available in this yfinance version)")

    # Print known categorical values
    known_cats = {
        "sector": ["Technology", "Healthcare", "Financial Services", "Consumer Cyclical",
                   "Industrials", "Communication Services", "Consumer Defensive",
                   "Energy", "Utilities", "Real Estate", "Basic Materials"],
        "region": ["us", "gb", "ca", "au", "de", "fr", "jp", "hk", "cn", "in"],
        "exchange": ["NMS", "NYQ", "NGM", "NCM", "ASE", "PCX"],
    }
    if args.field in known_cats:
        print(f"\nKnown values for '{args.field}':")
        for v in known_cats[args.field]:
            print(f"  {v}")


# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="screen.py",
        description="Stock screener using yfinance EquityQuery",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = p.add_subparsers(dest="command", required=True)

    # equity
    eq = sub.add_parser("equity", help="Screen equities with custom filters")
    eq.add_argument("--filter",   action="append", nargs="+", metavar="TOKEN",
                    help="Filter: KEY OP VALUE (e.g. --filter pe lt 15)")
    eq.add_argument("--count",    type=int, default=25, help="Max results (default 25)")
    eq.add_argument("--sort-by",  default=None, help="Sort field")
    eq.add_argument("--columns",  default=None, help="Comma-separated column list")

    # fund
    fu = sub.add_parser("fund", help="Screen mutual funds / ETFs")
    fu.add_argument("--filter",   action="append", nargs="+", metavar="TOKEN",
                    help="Filter: KEY OP VALUE")
    fu.add_argument("--count",    type=int, default=25, help="Max results (default 25)")
    fu.add_argument("--sort-by",  default=None, help="Sort field")

    # preset
    pr = sub.add_parser("preset", help="Run a named preset screen")
    pr.add_argument("name", choices=list(PRESETS.keys()))
    pr.add_argument("--count",   type=int, default=25, help="Max results (default 25)")
    pr.add_argument("--sort-by", default=None, help="Override sort field")

    # fields
    sub.add_parser("fields", help="List valid EquityQuery field names")

    # values
    va = sub.add_parser("values", help="List valid values for a categorical field")
    va.add_argument("field", help="Field name (e.g. sector, region)")

    return p


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    parser = build_parser()
    args = parser.parse_args()
    dispatch = {
        "equity":  cmd_equity,
        "fund":    cmd_fund,
        "preset":  cmd_preset,
        "fields":  cmd_fields,
        "values":  cmd_values,
    }
    try:
        dispatch[args.command](args)
    except Exception as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
