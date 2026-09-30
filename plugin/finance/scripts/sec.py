#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "httpx>=0.27",
#   "tabulate>=0.9",
#   "html2text>=2020.1",
#   "beautifulsoup4>=4.12",
#   "lxml>=5.0",
# ]
# ///
"""
sec.py — SEC EDGAR filing fetcher.

Fetches filing lists, metadata, and full-text content from the SEC EDGAR
public API (data.sec.gov). No API key required.

Usage:
    ./sec.py <command> [options]

Commands:
  cik         Look up CIK number for a ticker symbol
  filings     List recent filings for a company
  facts       XBRL financial facts for a company (structured data)
  fetch       Download and display the text of a specific filing
  search      Full-text search across all EDGAR filings (efts)

Examples:
    ./sec.py cik AAPL
    ./sec.py filings TSLA --type 10-K --count 5
    ./sec.py filings MSFT --type 10-Q
    ./sec.py facts NVDA --concept RevenueFromContractWithCustomerExcludingAssessedTax
    ./sec.py fetch AAPL --type 10-K --index 0
    ./sec.py search "artificial intelligence risk" --type 10-K --count 10
"""

import argparse
import json
import os
import re
import sys
from functools import lru_cache

import warnings

import httpx
import html2text
from bs4 import BeautifulSoup, XMLParsedAsHTMLWarning
from tabulate import tabulate

# EDGAR primary docs are XHTML; the lxml HTML parser handles them fine
warnings.filterwarnings("ignore", category=XMLParsedAsHTMLWarning)

# SEC requires a descriptive User-Agent naming a real contact, or it rate-limits.
HEADERS = {
    "User-Agent": os.environ.get("SEC_USER_AGENT", "finskills-research research@finskills.local"),
    "Accept-Encoding": "gzip, deflate",
}

TICKERS_URL  = "https://www.sec.gov/files/company_tickers.json"
SUBMISSIONS  = "https://data.sec.gov/submissions/CIK{cik}.json"
COMPANY_FACTS = "https://data.sec.gov/api/xbrl/companyfacts/CIK{cik}.json"
COMPANY_CONCEPT = "https://data.sec.gov/api/xbrl/companyconcept/CIK{cik}/{taxonomy}/{concept}.json"
EDGAR_ARCHIVES = "https://www.sec.gov/Archives/edgar/data/{cik}/{accession}/{doc}"
EFTS_SEARCH  = "https://efts.sec.gov/LATEST/search-index?q={query}&dateRange=custom&startdt={start}&enddt={end}&forms={form}&hits.hits.total.value=true&hits.hits._source.period_of_report=true"
EFTS_FULL    = "https://efts.sec.gov/LATEST/search-index?q={query}&forms={form}&hits.hits._source.file_date=true"


# ---------------------------------------------------------------------------
# HTTP helpers
# ---------------------------------------------------------------------------

def _get(url: str, params: dict | None = None) -> dict | str:
    try:
        r = httpx.get(url, headers=HEADERS, params=params, timeout=20, follow_redirects=True)
        r.raise_for_status()
        ct = r.headers.get("content-type", "")
        if "json" in ct:
            return r.json()
        return r.text
    except httpx.HTTPStatusError as e:
        print(f"HTTP {e.response.status_code} for {url}", file=sys.stderr)
        sys.exit(1)
    except httpx.RequestError as e:
        print(f"Request error: {e}", file=sys.stderr)
        sys.exit(1)


@lru_cache(maxsize=1)
def _load_tickers() -> dict:
    """Download the master ticker→CIK mapping (cached for the session)."""
    data = _get(TICKERS_URL)
    if isinstance(data, str):
        data = json.loads(data)
    # Remap: ticker.upper() → {cik, title}
    result = {}
    for _idx, entry in data.items():
        ticker = entry["ticker"].upper()
        result[ticker] = {"cik": str(entry["cik_str"]).zfill(10), "title": entry["title"]}
    return result


def _resolve_cik(ticker_or_cik: str) -> tuple[str, str]:
    """Return (padded_cik, company_name). Accepts ticker or numeric CIK."""
    v = ticker_or_cik.upper()
    if v.isdigit():
        return v.zfill(10), v
    tickers = _load_tickers()
    if v not in tickers:
        print(f"Ticker '{v}' not found in EDGAR. Try a CIK number directly.", file=sys.stderr)
        sys.exit(1)
    entry = tickers[v]
    return entry["cik"], entry["title"]


def _submissions(cik: str) -> dict:
    return _get(SUBMISSIONS.format(cik=cik))


def _recent_filings(sub: dict) -> dict:
    """Return the columnar 'recent' filings dict from a submissions response."""
    return sub.get("filings", {}).get("recent", {})


def _columnar_to_rows(recent: dict) -> list[dict]:
    """Convert EDGAR columnar format to list of row dicts."""
    keys = [k for k in recent.keys() if isinstance(recent[k], list)]
    if not keys:
        return []
    n = len(recent[keys[0]])
    return [{k: recent[k][i] for k in keys} for i in range(n)]


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def cmd_cik(args: argparse.Namespace) -> None:
    for ticker in args.tickers:
        cik, name = _resolve_cik(ticker)
        print(f"{ticker.upper():<10} CIK: {cik}  |  {name}")


def cmd_filings(args: argparse.Namespace) -> None:
    cik, name = _resolve_cik(args.ticker)
    sub  = _submissions(cik)
    rows = _columnar_to_rows(_recent_filings(sub))

    # Filter by form type
    if args.type:
        form_filter = args.type.upper()
        rows = [r for r in rows if r.get("form", "").upper() == form_filter]

    rows = rows[:args.count]
    if not rows:
        print(f"[no filings found for type={args.type}]")
        return

    table = [
        (
            r.get("filingDate", ""),
            r.get("form", ""),
            r.get("reportDate", ""),
            r.get("accessionNumber", ""),
            r.get("primaryDocument", "")[:40],
        )
        for r in rows
    ]
    print(f"\n=== {name} ({cik}) — Filings ===")
    print(tabulate(table, headers=["Filed", "Form", "Period", "Accession #", "Primary Doc"], tablefmt="github"))

    # Show tickers / exchange info from submissions
    tickers = sub.get("tickers", [])
    exchanges = sub.get("exchanges", [])
    sic = sub.get("sic", "")
    sic_desc = sub.get("sicDescription", "")
    print(f"\nTickers: {tickers}  |  Exchanges: {exchanges}  |  SIC: {sic} ({sic_desc})")


def cmd_facts(args: argparse.Namespace) -> None:
    cik, name = _resolve_cik(args.ticker)
    data = _get(COMPANY_FACTS.format(cik=cik))
    if isinstance(data, str):
        data = json.loads(data)

    if args.concept:
        # Search for the concept in us-gaap and dei taxonomies
        found = False
        for taxonomy in ("us-gaap", "dei", "ifrs-full"):
            tax_data = data.get("facts", {}).get(taxonomy, {})
            if args.concept in tax_data:
                concept_data = tax_data[args.concept]
                label = concept_data.get("label", args.concept)
                desc  = concept_data.get("description", "")[:200]
                print(f"\n=== {name} — {label} ({taxonomy}/{args.concept}) ===")
                if desc:
                    print(f"Description: {desc}\n")
                # Show annual (10-K) values in USD
                units = concept_data.get("units", {})
                for unit, obs_list in units.items():
                    annual = [o for o in obs_list if o.get("form") in ("10-K", "20-F", "40-F")]
                    annual.sort(key=lambda x: x.get("end", ""), reverse=True)
                    annual = annual[:args.count]
                    if annual:
                        rows = [(o.get("end"), o.get("val"), o.get("form"), o.get("filed")) for o in annual]
                        print(f"Unit: {unit}")
                        print(tabulate(rows, headers=["Period End", "Value", "Form", "Filed"], tablefmt="github"))
                found = True
                break
        if not found:
            print(f"Concept '{args.concept}' not found. Use --list-concepts to browse available concepts.")
    elif args.list_concepts:
        all_concepts = []
        for taxonomy in ("us-gaap", "dei"):
            concepts = data.get("facts", {}).get(taxonomy, {})
            for cname, cdata in list(concepts.items())[:200]:
                all_concepts.append((taxonomy, cname, cdata.get("label", "")[:60]))
        print(f"\n=== {name} — Available XBRL Concepts (first 200) ===")
        print(tabulate(all_concepts, headers=["Taxonomy", "Concept", "Label"], tablefmt="github"))
    else:
        # Summary: show available taxonomies and concept counts
        facts = data.get("facts", {})
        print(f"\n=== {name} ({cik}) — XBRL Facts Summary ===")
        for taxonomy, concepts in facts.items():
            print(f"  {taxonomy}: {len(concepts)} concepts")
        print("\nUse --concept <name> to fetch a specific metric.")
        print("Use --list-concepts to browse available concept names.")
        print("\nCommon concepts:")
        common = [
            "Revenues", "RevenueFromContractWithCustomerExcludingAssessedTax",
            "NetIncomeLoss", "EarningsPerShareDiluted",
            "Assets", "Liabilities", "StockholdersEquity",
            "NetCashProvidedByUsedInOperatingActivities",
            "PaymentsToAcquirePropertyPlantAndEquipment",
            "CommonStockSharesOutstanding",
        ]
        for c in common:
            print(f"  {c}")


# Inline-XBRL filings open with a large hidden block of taxonomy contexts and
# facts. html2text renders it as text, so without stripping it the first tens of
# thousands of characters are tags and the narrative is pushed past --chars.
_XBRL_TAGS = ("ix:header", "ix:hidden", "ix:references", "ix:resources",
              "script", "style", "xbrl", "link:schemaref")


def _html_to_text(raw: str) -> str:
    soup = BeautifulSoup(raw, "lxml")

    for name in _XBRL_TAGS:
        for el in soup.find_all(re.compile(re.escape(name), re.I)):
            el.decompose()

    for el in soup.find_all(style=re.compile(r"display\s*:\s*none", re.I)):
        el.decompose()

    h = html2text.HTML2Text()
    h.ignore_links = True
    h.ignore_images = True
    h.body_width = 120
    text = h.handle(str(soup))

    # Collapse the runs of blank lines and table padding EDGAR HTML leaves behind
    text = re.sub(r"[ \t]+\n", "\n", text)
    text = re.sub(r"\n{3,}", "\n\n", text)
    return text.strip()


def _extract_section(text: str, section: str) -> tuple[str, bool]:
    """Slice from a named Item heading to the next one. Skips the TOC entry."""
    pat = re.compile(
        rf"^[ \t>*_]*item\s*{re.escape(section)}\b[.\s:—-]*", re.I | re.M)
    starts = [m.start() for m in pat.finditer(text)]
    if not starts:
        return text, False

    # The first hit is usually the table of contents; prefer the last hit that
    # still leaves a substantial body after it.
    start = next((s for s in reversed(starts) if len(text) - s > 500), starts[-1])

    nxt = re.compile(r"^[ \t>*_]*item\s*\d+[A-Za-z]?\b[.\s:—-]", re.I | re.M)
    end = next((m.start() for m in nxt.finditer(text, start + 1) if m.start() > start + 200),
               len(text))
    return text[start:end].strip(), True


def cmd_fetch(args: argparse.Namespace) -> None:
    cik, name = _resolve_cik(args.ticker)
    sub  = _submissions(cik)
    rows = _columnar_to_rows(_recent_filings(sub))

    if args.type:
        rows = [r for r in rows if r.get("form", "").upper() == args.type.upper()]

    if not rows:
        print(f"[no filings found for type={args.type}]")
        return

    idx = args.index
    if idx >= len(rows):
        print(f"Index {idx} out of range (found {len(rows)} filings of type {args.type})")
        sys.exit(1)

    filing = rows[idx]
    accession = filing["accessionNumber"].replace("-", "")
    primary_doc = filing.get("primaryDocument", "")
    filed = filing.get("filingDate", "")
    form  = filing.get("form", "")
    period = filing.get("reportDate", "")

    print(f"\n=== {name} — {form} filed {filed} (period: {period}) ===")
    print(f"Accession: {filing['accessionNumber']}")

    if not primary_doc:
        print("[no primary document identified]")
        return

    url = EDGAR_ARCHIVES.format(cik=cik.lstrip("0"), accession=accession, doc=primary_doc)
    print(f"URL: {url}\n")

    raw = _get(url)
    if not isinstance(raw, str):
        raw = json.dumps(raw, indent=2)

    clean = _html_to_text(raw)

    if args.section:
        clean, found = _extract_section(clean, args.section)
        if not found:
            print(f"[section {args.section!r} not found — showing full document]\n")

    total = len(clean)
    limit = args.chars
    if total > limit:
        clean = clean[:limit] + (
            f"\n\n[... truncated at {limit:,} of {total:,} chars. "
            f"Use --chars to increase or --section to target ...]"
        )

    print(clean)


def cmd_search(args: argparse.Namespace) -> None:
    """Full-text search using EDGAR full-text search (efts.sec.gov)."""
    import urllib.parse
    query = urllib.parse.quote(args.query)
    form_param = args.type or ""
    url = f"https://efts.sec.gov/LATEST/search-index?q={query}&forms={form_param}&hits.hits.total.value=true"
    if args.count:
        url += f"&hits.hits._source.period_of_report=true&hits.hits._total=true"

    params = {
        "q": args.query,
        "forms": args.type or "",
        "dateRange": "custom" if (args.start or args.end) else "",
    }
    if args.start:
        params["startdt"] = args.start
    if args.end:
        params["enddt"] = args.end

    data = _get("https://efts.sec.gov/LATEST/search-index", params)
    if isinstance(data, str):
        try:
            data = json.loads(data)
        except json.JSONDecodeError:
            print(data[:2000])
            return

    hits = data.get("hits", {}).get("hits", [])
    total = data.get("hits", {}).get("total", {}).get("value", "?")

    if not hits:
        print("[no results]")
        return

    print(f"\n=== EDGAR Search: '{args.query}' — {total} total results ===")
    rows = []
    for h in hits[:args.count]:
        src = h.get("_source", {})
        rows.append((
            src.get("file_date", "")[:10],
            src.get("form_type", ""),
            src.get("display_names", [""])[0][:40] if src.get("display_names") else "",
            src.get("period_of_report", "")[:10],
        ))
    print(tabulate(rows, headers=["Filed", "Form", "Entity", "Period"], tablefmt="github"))


# ---------------------------------------------------------------------------
# Argument parsing
# ---------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        prog="sec.py",
        description="SEC EDGAR filing CLI",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    sub = p.add_subparsers(dest="command", required=True)

    # cik
    c = sub.add_parser("cik", help="Look up CIK for ticker(s)")
    c.add_argument("tickers", nargs="+")

    # filings
    f = sub.add_parser("filings", help="List recent filings")
    f.add_argument("ticker")
    f.add_argument("--type", default=None, help="Form type filter: 10-K, 10-Q, 8-K, etc.")
    f.add_argument("--count", type=int, default=20, help="Max results (default 20)")

    # facts
    fa = sub.add_parser("facts", help="XBRL financial facts")
    fa.add_argument("ticker")
    fa.add_argument("--concept", default=None, help="XBRL concept name (e.g. Revenues)")
    fa.add_argument("--list-concepts", action="store_true", help="List available concept names")
    fa.add_argument("--count", type=int, default=10, help="Number of observations (default 10)")

    # fetch
    fe = sub.add_parser("fetch", help="Download and display a filing")
    fe.add_argument("ticker")
    fe.add_argument("--type", default="10-K", help="Form type (default: 10-K)")
    fe.add_argument("--index", type=int, default=0, help="Which filing (0=most recent, default 0)")
    fe.add_argument("--chars", type=int, default=80000, help="Max characters to display (default 80000)")
    fe.add_argument("--section", help="Only show one item, e.g. --section 1A (Risk Factors), --section 7 (MD&A)")

    # search
    s = sub.add_parser("search", help="Full-text search across EDGAR filings")
    s.add_argument("query", help="Search terms")
    s.add_argument("--type", default=None, help="Form type: 10-K, 10-Q, 8-K, etc.")
    s.add_argument("--start", default=None, help="Start date YYYY-MM-DD")
    s.add_argument("--end",   default=None, help="End date YYYY-MM-DD")
    s.add_argument("--count", type=int, default=20, help="Max results (default 20)")

    return p


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    parser = build_parser()
    args = parser.parse_args()
    dispatch = {
        "cik":     cmd_cik,
        "filings": cmd_filings,
        "facts":   cmd_facts,
        "fetch":   cmd_fetch,
        "search":  cmd_search,
    }
    try:
        dispatch[args.command](args)
    except Exception as exc:
        print(f"Error: {exc}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
