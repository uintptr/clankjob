#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "yfinance>=0.2",
#   "pandas>=2.0",
#   "numpy>=1.26",
#   "statsmodels>=0.14",
# ]
# ///
"""
pairs.py — Cointegration/pairs-trading test between two securities.

Tests whether the spread between two tickers (e.g. XIC.TO vs ZCN.TO) is
mean-reverting: Engle-Granger cointegration on log prices, OLS hedge ratio,
ADF on residuals, spread amplitude, and current z-score.

Spreads between funds on the same underlying carry a deterministic drift from
the fee differential, which a constant-only ADF reads as a unit root. --trend
ct removes a linear trend first; auto picks ct when the drift is significant.

Usage:
    ./pairs.py <ticker_a> <ticker_b> [--period 3y] [--trend auto|c|ct]

Example:
    ./pairs.py XIC.TO ZCN.TO
    ./pairs.py HXS.TO VFV.TO --period 5y
    ./pairs.py FBTC.TO BTCC-B.TO --trend ct
"""
import argparse

import numpy as np
import pandas as pd
import statsmodels.api as sm
import yfinance as yf
from statsmodels.tsa.stattools import adfuller, coint


def half_life(s: pd.Series) -> float:
    lag = s.shift(1).dropna()
    d = (s - s.shift(1)).dropna()
    lam = sm.OLS(d.values, sm.add_constant(lag.values)).fit().params[1]
    return -np.log(2) / lam if lam < 0 else float("inf")


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("ticker_a")
    p.add_argument("ticker_b")
    p.add_argument("--period", default="3y", help="yfinance lookback (default 3y)")
    p.add_argument("--trend", default="auto", choices=["auto", "c", "ct"],
                   help="deterministic term in the spread (default auto)")
    args = p.parse_args()

    px = yf.download([args.ticker_a, args.ticker_b], period=args.period,
                     auto_adjust=True, progress=False)["Close"].dropna()
    a, b = args.ticker_a, args.ticker_b
    rets = px.pct_change().dropna()
    logp = np.log(px)

    t = np.arange(len(logp))
    cfit = sm.OLS(logp[a], sm.add_constant(logp[b])).fit()
    tfit = sm.OLS(cfit.resid.values, sm.add_constant(t)).fit()
    drift_t = tfit.tvalues[1]

    trend = args.trend
    if trend == "auto":
        trend = "ct" if abs(drift_t) > 2 else "c"

    if trend == "ct":
        X = sm.add_constant(pd.DataFrame({b: logp[b], "t": t}, index=logp.index))
        fit = sm.OLS(logp[a], X).fit()
        drift = fit.params["t"] * 252 * 100
    else:
        fit = cfit
        drift = tfit.params[1] * 252 * 100

    beta = fit.params[b]
    resid = fit.resid
    cg = coint(logp[a], logp[b], trend=trend)
    adf = adfuller(resid, regression="c")
    sd = resid.std()
    hl = half_life(resid)

    print(f"Pair:           {a} vs {b}  ({args.period} daily, n={len(px)})")
    print(f"Return corr:    {rets[a].corr(rets[b]):.4f}")
    print(f"Hedge beta:     {beta:.4f}")
    print(f"Trend model:    {trend}" + (f"  (auto: drift t={drift_t:+.1f})"
                                        if args.trend == "auto" else ""))
    print(f"Spread drift:   {drift:+.2f}%/yr")
    print(f"Coint p-value:  {cg[1]:.4f}")
    print(f"ADF p-value:    {adf[1]:.4f}")
    print(f"Residual sd:    {100*sd:.2f}%   2-sigma band: +/-{100*2*sd:.2f}%")
    print(f"Amplitude:      {100*(resid.max()-resid.min()):.2f}%")
    print(f"Half-life:      {hl:.1f} days" if np.isfinite(hl) else
          "Half-life:      n/a (no mean reversion)")
    print(f"z-score now:    {resid.iloc[-1]/sd:+.2f}")
    verdict = "cointegrated" if cg[1] < 0.05 else "NOT cointegrated"
    print(f"Verdict:        {verdict} (trend={trend})")
    if trend == "ct":
        favours = a if drift > 0 else b
        print(f"                {abs(drift):.2f}%/yr deterministic drift favouring "
              f"{favours} (fee gap, hedge cost, decay) — that part is a hold, "
              f"not a spread trade")
    if hl < 1:
        print(f"                half-life < 1 day: residual is likely bid-ask "
              f"bounce, not a tradeable spread")
    print(f"Cost check:     2-sigma entry is {100*2*sd:.2f}% gross; "
          f"needs round-trip cost + borrow below that to pay")


if __name__ == "__main__":
    main()
