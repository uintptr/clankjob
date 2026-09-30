#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "tabulate>=0.9",
# ]
# ///
"""
dcf.py — Discounted Cash Flow (DCF) calculator.

Accepts free cash flow inputs and model assumptions, then outputs:
  - Projected FCF for each year
  - Terminal value (Gordon Growth Model)
  - Enterprise value and implied equity value per share
  - Sensitivity table (WACC × terminal growth rate)

Usage:
    ./dcf.py [options]

Inputs (all optional — defaults produce an illustrative model):
    --fcf           Base free cash flow in millions USD (required or use --fcfs)
    --fcfs          Comma-separated FCF history, e.g. "100,120,145,160"
                    The last value is used as the base; others are shown for context.
    --growth        FCF growth rate for projection years, % (default: 10.0)
    --wacc          Weighted average cost of capital, % (default: 10.0)
    --terminal      Terminal growth rate, % (default: 2.5)
    --years         Projection horizon in years (default: 5)
    --net-debt      Net debt in millions USD (default: 0)
    --shares        Shares outstanding in millions (default: 100)
    --current-price Current share price USD (default: 0, skips upside calc)
    --ticker        Label for the output (e.g. AAPL)
    --stage2-growth Growth rate for years 6-10 in a two-stage model (optional)

Examples:
    # Simple single-stage DCF
    ./dcf.py --fcf 5000 --growth 12 --wacc 9 --terminal 3 --net-debt -10000 --shares 3900 --current-price 220 --ticker AAPL

    # Two-stage model (high growth then slowing)
    ./dcf.py --fcf 5000 --growth 20 --stage2-growth 8 --wacc 10 --terminal 2.5 --years 10

    # Pipe in FCF history to auto-compute CAGR as base growth rate
    ./dcf.py --fcfs "3100,3800,4200,5000" --wacc 9 --terminal 3 --shares 3900 --net-debt -10000
"""

import argparse
import sys
from tabulate import tabulate


# ---------------------------------------------------------------------------
# Core DCF engine
# ---------------------------------------------------------------------------

def _cagr(values: list[float]) -> float:
    """Compute CAGR from a list of values."""
    if len(values) < 2:
        return 0.0
    return (values[-1] / values[0]) ** (1 / (len(values) - 1)) - 1


def run_dcf(
    base_fcf: float,
    growth_rate: float,       # as decimal, e.g. 0.10
    wacc: float,              # as decimal
    terminal_growth: float,   # as decimal
    years: int,
    stage2_growth: float | None = None,  # optional second stage (decimal)
    stage2_start: int = 6,
) -> dict:
    """
    Core DCF engine. Returns a dict with projection details.
    """
    assert wacc > terminal_growth, "WACC must exceed terminal growth rate"

    projections = []
    pv_sum = 0.0
    fcf = base_fcf

    for yr in range(1, years + 1):
        # Determine growth rate for this year
        if stage2_growth is not None and yr >= stage2_start:
            g = stage2_growth
        else:
            g = growth_rate

        fcf = fcf * (1 + g)
        discount = (1 + wacc) ** yr
        pv = fcf / discount
        pv_sum += pv
        projections.append({
            "year": yr,
            "growth_rate": g,
            "fcf": fcf,
            "discount_factor": discount,
            "pv": pv,
        })

    # Terminal value (Gordon Growth Model) at end of projection
    terminal_fcf = projections[-1]["fcf"] * (1 + terminal_growth)
    terminal_value = terminal_fcf / (wacc - terminal_growth)
    pv_terminal = terminal_value / ((1 + wacc) ** years)

    enterprise_value = pv_sum + pv_terminal
    tv_pct = pv_terminal / enterprise_value * 100

    return {
        "projections": projections,
        "pv_sum": pv_sum,
        "terminal_value": terminal_value,
        "pv_terminal": pv_terminal,
        "enterprise_value": enterprise_value,
        "tv_pct_of_ev": tv_pct,
    }


def equity_value_per_share(ev: float, net_debt: float, shares: float) -> float | None:
    if shares <= 0:
        return None
    equity_value = ev - net_debt
    return equity_value / shares


def sensitivity_table(
    base_fcf: float,
    growth_rate: float,
    years: int,
    wacc_range: list[float],
    tgr_range: list[float],
    net_debt: float,
    shares: float,
) -> list[list]:
    """Returns a 2D grid: rows=WACC, cols=terminal growth rate."""
    header = ["WACC \\ TGR"] + [f"{tgr*100:.1f}%" for tgr in tgr_range]
    rows = [header]
    for w in wacc_range:
        row = [f"{w*100:.1f}%"]
        for tg in tgr_range:
            if w <= tg:
                row.append("N/A")
                continue
            result = run_dcf(base_fcf, growth_rate, w, tg, years)
            ev = result["enterprise_value"]
            if shares > 0:
                eps_val = equity_value_per_share(ev, net_debt, shares)
                row.append(f"${eps_val:,.0f}" if eps_val else "—")
            else:
                row.append(f"${ev/1e3:,.0f}B")
        rows.append(row)
    return rows


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def _fmt_m(v: float) -> str:
    """Format a value in millions, auto-scaling to B/M."""
    abs_v = abs(v)
    if abs_v >= 1_000:
        return f"${v/1_000:,.1f}B"
    return f"${v:,.0f}M"


def main() -> None:
    p = argparse.ArgumentParser(
        prog="dcf.py",
        description="DCF valuation calculator",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument("--fcf",           type=float, default=None,  help="Base FCF (millions USD)")
    p.add_argument("--fcfs",          type=str,   default=None,  help="Historical FCFs comma-separated (millions)")
    p.add_argument("--growth",        type=float, default=10.0,  help="FCF growth rate %% (default 10)")
    p.add_argument("--wacc",          type=float, default=10.0,  help="WACC %% (default 10)")
    p.add_argument("--terminal",      type=float, default=2.5,   help="Terminal growth rate %% (default 2.5)")
    p.add_argument("--years",         type=int,   default=5,     help="Projection years (default 5)")
    p.add_argument("--net-debt",      type=float, default=0.0,   help="Net debt millions USD (negative = net cash)")
    p.add_argument("--shares",        type=float, default=0.0,   help="Shares outstanding millions")
    p.add_argument("--current-price", type=float, default=0.0,   help="Current share price USD")
    p.add_argument("--ticker",        type=str,   default="",    help="Ticker label")
    p.add_argument("--stage2-growth", type=float, default=None,  help="Stage 2 growth rate %% (years 6+)")

    args = p.parse_args()

    # Resolve base FCF
    history: list[float] = []
    if args.fcfs:
        try:
            history = [float(x.strip()) for x in args.fcfs.split(",")]
        except ValueError:
            print("Error: --fcfs must be comma-separated numbers", file=sys.stderr)
            sys.exit(1)
        base_fcf = history[-1]
    elif args.fcf is not None:
        base_fcf = args.fcf
    else:
        print("Error: provide --fcf or --fcfs", file=sys.stderr)
        p.print_help()
        sys.exit(1)

    # Auto-compute growth from history if not overridden
    growth_rate = args.growth / 100
    if history and len(history) >= 2 and args.growth == 10.0:
        cagr = _cagr(history)
        print(f"Note: auto-computed FCF CAGR from history = {cagr*100:.1f}%. "
              f"Override with --growth if needed.\n")
        growth_rate = cagr

    wacc          = args.wacc / 100
    terminal_rate = args.terminal / 100
    stage2        = (args.stage2_growth / 100) if args.stage2_growth is not None else None

    if wacc <= terminal_rate:
        print("Error: WACC must be greater than terminal growth rate", file=sys.stderr)
        sys.exit(1)

    label = args.ticker or "DCF Model"

    # ---- Run model ----
    result = run_dcf(base_fcf, growth_rate, wacc, terminal_rate, args.years, stage2)

    # ---- Print ----
    print(f"\n{'='*60}")
    print(f"  {label} — DCF Valuation")
    print(f"{'='*60}")

    # Assumptions
    assumptions = [
        ("Base FCF",             _fmt_m(base_fcf)),
        ("Growth Rate (Yr 1–5)", f"{growth_rate*100:.1f}%"),
    ]
    if stage2 is not None:
        assumptions.append(("Growth Rate (Yr 6+)", f"{stage2*100:.1f}%"))
    assumptions += [
        ("Projection Years",     str(args.years)),
        ("WACC",                 f"{wacc*100:.1f}%"),
        ("Terminal Growth Rate", f"{terminal_rate*100:.1f}%"),
    ]
    if args.net_debt != 0:
        assumptions.append(("Net Debt / (Cash)", _fmt_m(args.net_debt)))
    if args.shares > 0:
        assumptions.append(("Shares Outstanding", f"{args.shares:,.0f}M"))

    print("\nAssumptions:")
    print(tabulate(assumptions, tablefmt="github"))

    # Historical FCF if provided
    if history:
        hist_rows = [(f"Year -{len(history)-1-i}", _fmt_m(v)) for i, v in enumerate(history)]
        print("\nHistorical FCF:")
        print(tabulate(hist_rows, headers=["Period", "FCF"], tablefmt="github"))

    # Projections
    proj_rows = [
        (
            f"Year {r['year']}",
            f"{r['growth_rate']*100:.1f}%",
            _fmt_m(r["fcf"]),
            f"{r['discount_factor']:.4f}",
            _fmt_m(r["pv"]),
        )
        for r in result["projections"]
    ]
    print("\nProjected Free Cash Flows:")
    print(tabulate(
        proj_rows,
        headers=["Year", "Growth", "FCF", "Discount Factor", "PV of FCF"],
        tablefmt="github"
    ))

    # Valuation summary
    ev = result["enterprise_value"]
    summary = [
        ("PV of FCFs",             _fmt_m(result["pv_sum"])),
        ("Terminal Value (TV)",    _fmt_m(result["terminal_value"])),
        ("PV of Terminal Value",   _fmt_m(result["pv_terminal"])),
        ("TV as % of EV",          f"{result['tv_pct_of_ev']:.1f}%"),
        ("Enterprise Value",       _fmt_m(ev)),
    ]
    if args.net_debt != 0:
        equity_val = ev - args.net_debt
        summary.append(("Less: Net Debt / (Cash)", _fmt_m(args.net_debt)))
        summary.append(("Equity Value",            _fmt_m(equity_val)))
    if args.shares > 0:
        per_share = equity_value_per_share(ev, args.net_debt, args.shares)
        summary.append(("Implied Share Price",     f"${per_share:,.2f}"))
        if args.current_price > 0:
            upside = (per_share / args.current_price - 1) * 100
            summary.append(("Current Price",       f"${args.current_price:,.2f}"))
            summary.append(("Upside / (Downside)", f"{upside:+.1f}%"))

    print("\nValuation Summary:")
    print(tabulate(summary, tablefmt="github"))

    # Sensitivity table
    half = 0.02  # ±2% for WACC, ±1% for TGR
    wacc_range = [round(wacc - half + i * 0.01, 4) for i in range(5)]  # wacc ±2% in 1% steps
    tgr_range  = [round(terminal_rate - 0.01 + i * 0.005, 4) for i in range(5)]  # tgr ±1%

    sens = sensitivity_table(
        base_fcf, growth_rate, args.years,
        wacc_range, tgr_range,
        args.net_debt, args.shares,
    )
    metric = "Implied Share Price" if args.shares > 0 else "Enterprise Value"
    print(f"\nSensitivity Table — {metric} (WACC rows, Terminal Growth Rate cols):")
    print(tabulate(sens[1:], headers=sens[0], tablefmt="github"))


if __name__ == "__main__":
    main()
