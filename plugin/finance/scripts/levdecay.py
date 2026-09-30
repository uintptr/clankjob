#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "yfinance>=0.2",
#   "pandas>=2.0",
#   "numpy>=1.26",
# ]
# ///
"""
levdecay.py — Leveraged-ETF volatility-decay harvest backtest.

Measures the systematic drift between a leveraged ETF (e.g. HSU) and the
equivalent leveraged position in its underlying index ETF (e.g. 2x VFV),
then backtests the "short leveraged + long Lx underlying" trade gross of
borrow costs.

Usage:
    ./levdecay.py <leveraged_ticker> <underlying_ticker> [--leverage 2] [--period 5y]

Example:
    ./levdecay.py HSU.TO VFV.TO
    ./levdecay.py HSU.TO VFV.TO --period 10y
"""
import argparse

import numpy as np
import pandas as pd
import yfinance as yf


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("lev")
    p.add_argument("base")
    p.add_argument("--leverage", type=float, default=2.0)
    p.add_argument("--period", default="5y")
    args = p.parse_args()

    px = yf.download([args.lev, args.base], period=args.period,
                     auto_adjust=True, progress=False)["Close"].dropna()
    r = px.pct_change().dropna()

    # trade: short lev, long L x base
    excess = (args.leverage * r[args.base] - r[args.lev]).dropna()
    ann_ret = excess.mean() * 252
    ann_vol = excess.std() * np.sqrt(252)
    cum = (1 + excess).cumprod()
    dd = (cum / cum.cummax()).min()

    print(f"Trade: short {args.lev} + long {args.leverage}x {args.base}"
          f"  ({args.period} daily, gross of borrow)")
    print(f"Annualized excess: {ann_ret*100:6.2f}%")
    print(f"Hedge-noise vol:   {ann_vol*100:6.2f}%")
    print(f"Sharpe (gross):    {ann_ret/ann_vol:6.2f}")
    print(f"Max drawdown:      {(1-dd)*100:6.2f}%")


if __name__ == "__main__":
    main()
