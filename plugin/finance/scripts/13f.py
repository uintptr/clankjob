#!/usr/bin/env python3
"""
13f.py — 13F-HR holdings and quarter-over-quarter deltas from SEC EDGAR.

Fetches Form 13F-HR information tables for institutional investment
managers, normalises each position to a percentage of that manager's
reported 13F AUM, and reports quarter-over-quarter changes. No API key
required; stdlib only.

Usage:
    ./13f.py <command> [options]

The roster of managers is read from 13f_filings/roster.json (override the
path with FINSKILLS_13F_ROSTER). Edit that file to add or drop managers;
a copy is embedded here as a fallback so the script still runs on its own.

Commands:
  roster      List the watchlist of 13F filers
  filings     List 13F-HR filings for a manager
  holdings    Show one quarter's positions, weighted by % of 13F AUM
  deltas      Quarter-over-quarter position changes for one manager
  consensus   Aggregate holdings across managers (crowding / holder counts)
  history     One manager's position weights across N quarters
  crowding    One security's holders and weights across managers over time

Trend commands take --csv PATH (raw numbers for a spreadsheet) and
--html PATH (a standalone chart page that needs no network to open).

Examples:
    ./13f.py roster
    ./13f.py roster --tier A
    ./13f.py filings berkshire --count 8
    ./13f.py holdings baupost --top 20
    ./13f.py deltas berkshire --top 25
    ./13f.py deltas 0001067983 --from 2025-12-31 --to 2026-06-30
    ./13f.py consensus --tier A --top 30
    ./13f.py consensus --funds berkshire,baupost,akre --compare
    ./13f.py history akre --quarters 8
    ./13f.py history berkshire --quarters 12 --csv bh.csv --html bh.html
    ./13f.py crowding --cusip 02079K107 --tier A
    ./13f.py crowding --issuer "alphabet" --quarters 8 --html goog.html

A manager is named either by its roster slug ("berkshire") or by a raw
10-digit CIK ("0001067983"), so managers outside the roster work too.

Caveats this tool cannot fix for you:
  * 13F is long-only and omits cash, shorts, and non-13(f) securities.
    A large long may be one leg of a hedge you cannot see.
  * Filings are up to 45 days stale at publication.
  * Options are reported at notional, which inflates apparent size. They
    are kept separate here and tagged in the CALL/PUT column.
  * Share counts are not split-adjusted, so a split shows up as a large
    ADD. Check the issuer before believing a >50% share change.
  * Values were reported in thousands before 2023-01-03 and in whole
    dollars from that date, but filers get this wrong (Baupost and
    Duquesne still filed thousands in 2026). The scale is inferred per
    filing from the implied price per share, not from the date, and
    everything is normalised to dollars.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import os
import sys
import time
import urllib.error
import urllib.request
import xml.etree.ElementTree as ET
from dataclasses import dataclass

# SEC requires a descriptive User-Agent naming a real contact.
USER_AGENT = os.environ.get(
    "SEC_USER_AGENT",
    "finskills-research research@finskills.local",
)

SUBMISSIONS = "https://data.sec.gov/submissions/CIK{cik}.json"
SUBMISSION_PAGE = "https://data.sec.gov/submissions/{name}"
ARCHIVE_DIR = "https://www.sec.gov/Archives/edgar/data/{cik}/{accession}"

# SEC asks for no more than 10 requests/second.
MIN_REQUEST_INTERVAL = 0.12

# Form 13F values switched from thousands to whole dollars for filings
# made on or after this date (SEC Release 34-95148).
DOLLARS_FROM = "2023-01-03"

# The 13F information table namespace has changed URI across schema
# revisions, so every lookup below matches on local name only ("{*}tag").
INFO_TABLE_TAG = "informationTable"


# ---------------------------------------------------------------------------
# JSON helpers
# ---------------------------------------------------------------------------

def _as_dict(value: object) -> dict[str, object]:
    """Narrow a decoded JSON value to a mapping, or an empty one."""
    if isinstance(value, dict):
        return {str(key): item for key, item in value.items()}
    return {}


def _as_list(value: object) -> list[object]:
    """Narrow a decoded JSON value to a list, or an empty one."""
    if isinstance(value, list):
        return list(value)
    return []


# ---------------------------------------------------------------------------
# Roster
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class Fund:
    slug: str
    cik: str
    name: str
    tier: str
    note: str


DEFAULT_TIER_LABELS = {
    "A": "13F approximates the whole book",
    "B": "Quality compounders / long duration",
    "C": "Activists (catalyst signal)",
    "D": "Growth / tech theme",
    "E": "Macro (positions express a view)",
}

# Editable copy of the roster, loaded in preference to the embedded one
# below. Relative to the repo root; override with FINSKILLS_13F_ROSTER.
ROSTER_FILE = "13f_filings/roster.json"

# Fallback so the script still works when copied off on its own.
EMBEDDED_ROSTER: list[Fund] = [
    Fund("berkshire", "0001067983", "Berkshire Hathaway Inc",
         "A", "Buffett / Abel / Combs / Weschler"),
    Fund("baupost", "0001061768", "Baupost Group LLC/MA", "A", "Seth Klarman"),
    Fund("himalaya", "0001709323", "Himalaya Capital Management LLC", "A", "Li Lu"),
    Fund("akre", "0001112520", "Akre Capital Management LLC",
         "A", "Chuck Akre successors"),
    Fund("ruane", "0001720792", "Ruane, Cunniff & Goldfarb L.P.", "A", "Sequoia Fund"),
    Fund("southeastern", "0000807985", "Southeastern Asset Management Inc",
         "A", "Mason Hawkins / Longleaf"),
    Fund("giverny", "0001641864", "Giverny Capital Inc.",
         "A", "Francois Rochon, Montreal"),
    Fund("chou", "0001389403", "Chou Associates Management Inc.",
         "A", "Francis Chou, Toronto"),

    Fund("fundsmith", "0001569205", "Fundsmith LLP", "B", "Terry Smith"),
    Fund("lindsell", "0001484150", "Lindsell Train Ltd", "B", "Nick Train"),
    Fund("polen", "0001034524", "Polen Capital Management LLC",
         "B", "growth-quality"),
    Fund("russo", "0000860643", "Gardner Russo & Quinn LLC",
         "B", "Tom Russo, Semper Vic"),
    Fund("tweedy", "0000732905", "Tweedy, Browne Co LLC",
         "B", "deep value lineage"),
    Fund("dodgecox", "0000200217", "Dodge & Cox", "B", "committee-run value"),

    Fund("elliott", "0001791786",
         "Elliott Investment Management L.P.", "C", "Paul Singer"),
    Fund("valueact", "0001418814", "ValueAct Holdings, L.P.", "C", "Mason Morfit"),
    Fund("starboard", "0001517137", "Starboard Value LP", "C", "Jeff Smith"),
    Fund("trian", "0001345471", "Trian Fund Management, L.P.", "C", "Nelson Peltz"),
    Fund("pershing", "0001336528",
         "Pershing Square Capital Management, L.P.", "C", "Bill Ackman"),

    Fund("tiger", "0001167483", "Tiger Global Management LLC",
         "D", "crossover growth"),
    Fund("coatue", "0001135730", "Coatue Management LLC", "D", "tech long/short"),
    Fund("lonepine", "0001061165", "Lone Pine Capital LLC", "D", "Tiger cub"),
    Fund("altimeter", "0001541617",
         "Altimeter Capital Management, LP", "D", "Brad Gerstner"),

    Fund("duquesne", "0001536411", "Duquesne Family Office LLC",
         "E", "Stanley Druckenmiller"),
    Fund("soros", "0001029160", "Soros Fund Management LLC",
         "E", "macro / thematic"),
]


def _roster_path() -> str | None:
    """Locate the roster file: env override, then repo-relative default."""
    override = os.environ.get("FINSKILLS_13F_ROSTER")
    if override:
        return override if os.path.exists(override) else None
    repo_root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    candidate = os.path.join(repo_root, ROSTER_FILE)
    return candidate if os.path.exists(candidate) else None


def load_roster() -> tuple[list[Fund], dict[str, str], str]:
    """Return (managers, tier labels, provenance) from file or the fallback."""
    path = _roster_path()
    if path is None:
        return EMBEDDED_ROSTER, DEFAULT_TIER_LABELS, "built into 13f.py"

    try:
        with open(path, "r", encoding="utf-8") as handle:
            doc = _as_dict(json.load(handle))
    except (OSError, json.JSONDecodeError) as exc:
        print(f"warning: ignoring unreadable {path}: {exc}", file=sys.stderr)
        return EMBEDDED_ROSTER, DEFAULT_TIER_LABELS, "built into 13f.py"

    funds: list[Fund] = []
    for raw in _as_list(doc.get("managers")):
        entry = _as_dict(raw)
        slug = str(entry.get("slug", "")).strip().lower()
        cik = str(entry.get("cik", "")).strip()
        if not slug or not cik.isdigit():
            print(
                f"warning: skipping malformed roster entry {entry!r}", file=sys.stderr)
            continue
        funds.append(Fund(
            slug=slug,
            cik=cik.zfill(10),
            name=str(entry.get("name", slug)),
            tier=str(entry.get("tier", "?")),
            note=str(entry.get("note", "")),
        ))

    if not funds:
        print(f"warning: no usable managers in {path}", file=sys.stderr)
        return EMBEDDED_ROSTER, DEFAULT_TIER_LABELS, "built into 13f.py"

    labels = {k: str(v) for k, v in _as_dict(doc.get("tiers")).items()}
    verified = str(doc.get("verified", "")).strip()
    provenance = path + (f" (CIKs verified {verified})" if verified else "")
    return funds, labels or DEFAULT_TIER_LABELS, provenance


ROSTER, TIER_LABELS, ROSTER_SOURCE = load_roster()
ROSTER_BY_SLUG = {f.slug: f for f in ROSTER}


# ---------------------------------------------------------------------------
# HTTP
# ---------------------------------------------------------------------------

_last_request_at = 0.0


def _throttle() -> None:
    """Space requests out to stay inside SEC's 10 req/s guidance."""
    global _last_request_at
    delta = time.monotonic() - _last_request_at
    if delta < MIN_REQUEST_INTERVAL:
        time.sleep(MIN_REQUEST_INTERVAL - delta)
    _last_request_at = time.monotonic()


def _cache_dir() -> str:
    base = os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache")
    return os.path.join(base, "finskills-13f")


def _cache_path(url: str) -> str:
    digest = hashlib.sha256(url.encode("utf-8")).hexdigest()[:32]
    return os.path.join(_cache_dir(), digest)


def _http_get(url: str, use_cache: bool = True) -> bytes:
    """GET a URL, with an on-disk cache. EDGAR archives are immutable."""
    path = _cache_path(url)
    if use_cache and os.path.exists(path):
        with open(path, "rb") as handle:
            return handle.read()

    _throttle()
    request = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
    last_error = ""
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=30) as response:
                body: bytes = response.read()
            break
        except urllib.error.HTTPError as exc:
            # 429/503 mean we are being throttled; anything else is fatal.
            if exc.code not in (429, 503) or attempt == 2:
                raise SystemExit(f"HTTP {exc.code} for {url}")
            last_error = f"HTTP {exc.code}"
            time.sleep(2.0 * (attempt + 1))
        except OSError as exc:
            if attempt == 2:
                raise SystemExit(f"Request failed for {url}: {exc}")
            last_error = str(exc)
            time.sleep(1.0 * (attempt + 1))
    else:
        raise SystemExit(f"Request failed for {url}: {last_error}")

    if use_cache:
        os.makedirs(_cache_dir(), exist_ok=True)
        with open(path, "wb") as handle:
            handle.write(body)
    return body


def _get_json(url: str, use_cache: bool = True) -> dict[str, object]:
    raw = _http_get(url, use_cache=use_cache)
    try:
        parsed = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise SystemExit(f"Bad JSON from {url}: {exc}")
    if not isinstance(parsed, dict):
        raise SystemExit(f"Expected a JSON object from {url}")
    return parsed


# ---------------------------------------------------------------------------
# Filing discovery
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class Filing:
    cik: str
    form: str
    filed: str
    period: str
    accession: str

    @property
    def is_amendment(self) -> bool:
        return self.form.endswith("/A")

    @property
    def values_in_dollars(self) -> bool:
        return self.filed >= DOLLARS_FROM


def resolve_fund(ident: str) -> Fund:
    """Accept a roster slug or a raw CIK."""
    key = ident.strip().lower()
    if key in ROSTER_BY_SLUG:
        return ROSTER_BY_SLUG[key]

    digits = key.lstrip("cik").lstrip("-_ ")
    if digits.isdigit():
        cik = digits.zfill(10)
        for fund in ROSTER:
            if fund.cik == cik:
                return fund
        return Fund(cik, cik, _lookup_name(cik), "?", "not in roster")

    raise SystemExit(
        f"Unknown manager '{ident}'. Use a roster slug (see `13f.py roster`) or a CIK."
    )


def _lookup_name(cik: str) -> str:
    submissions = _get_json(SUBMISSIONS.format(cik=cik), use_cache=False)
    name = submissions.get("name")
    return name if isinstance(name, str) else cik


def _rows_from_columnar(block: dict[str, object]) -> list[dict[str, object]]:
    """EDGAR returns filings as parallel arrays; zip them into records."""
    columns = {key: _as_list(value) for key, value in block.items()}
    keys = list(columns.keys())
    if not keys:
        return []
    length = len(columns[keys[0]])
    return [{k: columns[k][i] for k in keys} for i in range(length)]


def list_filings(cik: str, all_history: bool = False) -> list[Filing]:
    """Return every 13F-HR (and 13F-HR/A) filing, newest first."""
    submissions = _get_json(SUBMISSIONS.format(cik=cik), use_cache=False)
    all_filings = _as_dict(submissions.get("filings"))
    blocks = [_as_dict(all_filings.get("recent"))]

    if all_history:
        for extra in _as_list(all_filings.get("files")):
            name = _as_dict(extra).get("name")
            if isinstance(name, str) and name:
                blocks.append(_get_json(SUBMISSION_PAGE.format(name=name)))

    filings: list[Filing] = []
    for block in blocks:
        for row in _rows_from_columnar(block):
            form = str(row.get("form", ""))
            if not form.startswith("13F-HR"):
                continue
            filings.append(Filing(
                cik=cik,
                form=form,
                filed=str(row.get("filingDate", "")),
                period=str(row.get("reportDate", "")),
                accession=str(row.get("accessionNumber", "")),
            ))

    filings.sort(key=lambda f: (f.period, f.filed), reverse=True)
    return filings


def latest_per_period(filings: list[Filing]) -> dict[str, Filing]:
    """Collapse to one filing per period, preferring the newest amendment."""
    chosen: dict[str, Filing] = {}
    for filing in filings:
        current = chosen.get(filing.period)
        if current is None or filing.filed > current.filed:
            chosen[filing.period] = filing
    return chosen


# ---------------------------------------------------------------------------
# Information table parsing
# ---------------------------------------------------------------------------

@dataclass
class Position:
    issuer: str
    title: str
    cusip: str
    value: float          # normalised to whole dollars
    shares: float
    share_type: str       # SH (shares) or PRN (principal)
    put_call: str         # "", CALL, or PUT

    @property
    def key(self) -> tuple[str, str, str]:
        """Options are tracked apart from the underlying common."""
        return (self.cusip, self.put_call, self.share_type)


def _text(node: ET.Element | None, tag: str) -> str:
    if node is None:
        return ""
    found = node.find(f"{{*}}{tag}")
    if found is None or found.text is None:
        return ""
    return found.text.strip()


def _number(raw: str) -> float:
    if not raw:
        return 0.0
    try:
        return float(raw.replace(",", ""))
    except ValueError:
        return 0.0


def _candidate_urls(filing: Filing) -> list[str]:
    """XML documents in a filing's archive folder, likeliest table first."""
    nodash = filing.accession.replace("-", "")
    base = ARCHIVE_DIR.format(cik=int(filing.cik), accession=nodash)
    index = _get_json(f"{base}/index.json")

    items = _as_list(_as_dict(index.get("directory")).get("item"))
    names = [
        str(_as_dict(item).get("name", ""))
        for item in items
        if str(_as_dict(item).get("name", "")).lower().endswith(".xml")
    ]
    # primary_doc.xml is the cover page, never the holdings.
    names = [n for n in names if n.lower() != "primary_doc.xml"]
    # Filers name the table inconsistently, so only bias the order here;
    # fetch_positions confirms by inspecting each document's root tag.
    names.sort(key=lambda n: ("info" not in n.lower(), n))
    return [f"{base}/{n}" for n in names]


def _parse_info_table(payload: bytes, scale: float) -> list[Position] | None:
    """Parse an information table, or None if this is some other document."""
    try:
        root = ET.fromstring(payload)
    except ET.ParseError:
        return None
    if not root.tag.endswith(INFO_TABLE_TAG):
        return None

    positions: list[Position] = []
    for entry in root.findall("{*}infoTable"):
        amount = entry.find("{*}shrsOrPrnAmt")
        positions.append(Position(
            issuer=_text(entry, "nameOfIssuer"),
            title=_text(entry, "titleOfClass"),
            cusip=_text(entry, "cusip").upper(),
            value=_number(_text(entry, "value")) * scale,
            shares=_number(_text(amount, "sshPrnamt")),
            share_type=_text(amount, "sshPrnamtType") or "SH",
            put_call=_text(entry, "putCall").upper(),
        ))
    return positions


def _detect_scale(positions: list[Position], filing: Filing) -> float:
    """Work out whether `value` is in dollars or thousands.

    The rule says filings made from 2023-01-03 report whole dollars, but
    filers do get this wrong — Baupost and Duquesne were still reporting
    thousands in 2026. So infer it from the data instead: value / shares
    must land on a plausible share price. A median under $1 means the
    values are 1000x too small. Falls back to the date rule when there
    are no ordinary share rows to measure.
    """
    prices = sorted(
        p.value / p.shares
        for p in positions
        if p.share_type == "SH" and p.shares > 0 and p.value > 0
    )
    if prices:
        median = prices[len(prices) // 2]
        return 1000.0 if median < 1.0 else 1.0
    return 1.0 if filing.values_in_dollars else 1000.0


def fetch_positions(filing: Filing) -> list[Position]:
    """Download and parse one filing's information table."""
    for url in _candidate_urls(filing):
        parsed = _parse_info_table(_http_get(url), 1.0)
        if parsed:
            scale = _detect_scale(parsed, filing)
            if scale != 1.0:
                for position in parsed:
                    position.value *= scale
            return parsed
    return []


def consolidate(positions: list[Position]) -> dict[tuple[str, str, str], Position]:
    """Managers often report one issuer across several rows; sum them."""
    merged: dict[tuple[str, str, str], Position] = {}
    for position in positions:
        existing = merged.get(position.key)
        if existing is None:
            merged[position.key] = Position(**vars(position))
        else:
            existing.value += position.value
            existing.shares += position.shares
    return merged


def load_quarter(filing: Filing) -> tuple[dict[tuple[str, str, str], Position], float]:
    """Return consolidated positions for a filing plus its total value."""
    merged = consolidate(fetch_positions(filing))
    total = sum(p.value for p in merged.values())
    return merged, total


# ---------------------------------------------------------------------------
# Formatting
# ---------------------------------------------------------------------------

def _money(value: float) -> str:
    absolute = abs(value)
    if absolute >= 1e9:
        return f"${value / 1e9:,.1f}B"
    if absolute >= 1e6:
        return f"${value / 1e6:,.1f}M"
    if absolute >= 1e3:
        return f"${value / 1e3:,.1f}K"
    return f"${value:,.0f}"


def _shares(value: float) -> str:
    absolute = abs(value)
    if absolute >= 1e6:
        return f"{value / 1e6:,.2f}M"
    if absolute >= 1e3:
        return f"{value / 1e3:,.1f}K"
    return f"{value:,.0f}"


def _table(headers: list[str], rows: list[list[str]], aligns: str) -> None:
    """Print a fixed-width table. `aligns` is one char per column: l or r."""
    if not rows:
        print("  [no rows]")
        return
    widths = [len(h) for h in headers]
    for row in rows:
        for i, cell in enumerate(row):
            widths[i] = max(widths[i], len(cell))

    def render(cells: list[str]) -> str:
        parts = []
        for i, cell in enumerate(cells):
            if aligns[i] == "r":
                parts.append(cell.rjust(widths[i]))
            else:
                parts.append(cell.ljust(widths[i]))
        return "  ".join(parts).rstrip()

    print(render(headers))
    print("  ".join("-" * w for w in widths))
    for row in rows:
        print(render(row))


def _header(title: str) -> None:
    print()
    print(f"=== {title} ===")


def _disclaimer() -> None:
    print()
    print("Source: SEC EDGAR Form 13F-HR. Long-only, up to 45 days stale, "
          "excludes cash/shorts/non-13(f) securities.")
    print("Informational only; not investment advice. Verify against the "
          "filing before acting.")


# ---------------------------------------------------------------------------
# Trend series and export
# ---------------------------------------------------------------------------

SPARK_TICKS = "▁▂▃▄▅▆▇█"


@dataclass
class Series:
    """One row of a trend table: a label plus a value per period.

    `covered` marks the periods the source actually reported on. A value
    of None means "not held" only where covered is True; where it is
    False the manager simply had not filed, which is not a sell signal.
    """
    label: str
    key: str
    values: list[float | None]
    covered: list[bool] | None = None

    def is_covered(self, index: int) -> bool:
        return self.covered is None or self.covered[index]

    @property
    def latest(self) -> float | None:
        for value in reversed(self.values):
            if value is not None:
                return value
        return None

    @property
    def final(self) -> float | None:
        """Value in the most recent period; None if not held then."""
        return self.values[-1] if self.values else None

    @property
    def final_is_covered(self) -> bool:
        """False when the most recent quarter has no filing from this source."""
        return bool(self.values) and self.is_covered(len(self.values) - 1)

    @property
    def first(self) -> float | None:
        for value in self.values:
            if value is not None:
                return value
        return None

    @property
    def peak(self) -> float:
        present = [v for v in self.values if v is not None]
        return max(present) if present else 0.0


def _sparkline(values: list[float | None], ceiling: float,
               covered: list[bool] | None = None) -> str:
    """Blocks per period; '.' is not held, '?' is a quarter not filed."""
    if ceiling <= 0:
        return "." * len(values)
    out: list[str] = []
    for index, value in enumerate(values):
        if value is None:
            out.append("." if covered is None or covered[index] else "?")
            continue
        step = round((value / ceiling) * (len(SPARK_TICKS) - 1))
        out.append(SPARK_TICKS[max(0, min(step, len(SPARK_TICKS) - 1))])
    return "".join(out)


def _final_cell(series: Series) -> str:
    """Latest-period cell: a weight, an exit, or 'no filing' if unreported."""
    if series.final is not None:
        return f"{series.final:.2f}%"
    return "exited" if series.final_is_covered else "no filing"


def _write_csv(path: str, headers: list[str], rows: list[list[str]]) -> None:
    """Write raw values for a spreadsheet. Numbers stay unformatted."""
    with open(path, "w", encoding="utf-8", newline="") as handle:
        writer = csv.writer(handle)
        writer.writerow(headers)
        writer.writerows(rows)
    print(f"\nwrote {path} ({len(rows)} rows)")


def _svg_chart(title: str, periods: list[str], series: list[Series],
               unit: str) -> str:
    """A self-contained line chart. No external assets, so it opens offline."""
    width, height = 900, 420
    left, right, top, bottom = 70, 210, 44, 56
    plot_w = width - left - right
    plot_h = height - top - bottom
    ceiling = max([s.peak for s in series] or [0.0]) or 1.0
    steps = max(len(periods) - 1, 1)

    def x_at(i: int) -> float:
        return left + (plot_w * i / steps)

    def y_at(value: float) -> float:
        return top + plot_h - (plot_h * value / ceiling)

    palette = ["#2563eb", "#dc2626", "#059669", "#d97706", "#7c3aed",
               "#0891b2", "#be185d", "#4d7c0f", "#b45309", "#4338ca"]

    parts: list[str] = [
        (f'<svg xmlns="http://www.w3.org/2000/svg" '
         f'viewBox="0 0 {width} {height}" width="100%" role="img" '
         f'aria-label="{_esc(title)}">'),
        (f'<text x="{left}" y="26" font-size="16" font-weight="600" '
         f'fill="currentColor">{_esc(title)}</text>'),
    ]

    for tick in range(5):
        value = ceiling * tick / 4
        y = y_at(value)
        parts.append(
            f'<line x1="{left}" y1="{y:.1f}" x2="{left + plot_w}" y2="{y:.1f}" '
            f'stroke="currentColor" stroke-opacity="0.15" />')
        parts.append(
            f'<text x="{left - 8}" y="{y + 4:.1f}" font-size="11" '
            f'text-anchor="end" fill="currentColor" fill-opacity="0.7">'
            f'{value:.1f}{_esc(unit)}</text>')

    for i, period in enumerate(periods):
        parts.append(
            f'<text x="{x_at(i):.1f}" y="{top + plot_h + 20}" font-size="11" '
            f'text-anchor="middle" fill="currentColor" fill-opacity="0.7">'
            f'{_esc(period[:7])}</text>')

    for index, entry in enumerate(series):
        colour = palette[index % len(palette)]
        # Split on gaps so a quarter that was not held leaves a break.
        run: list[str] = []
        for i, value in enumerate(entry.values):
            if value is None:
                if len(run) > 1:
                    parts.append(
                        f'<polyline fill="none" stroke="{colour}" '
                        f'stroke-width="2" points="{" ".join(run)}" />')
                run = []
                continue
            run.append(f"{x_at(i):.1f},{y_at(value):.1f}")
            parts.append(
                f'<circle cx="{x_at(i):.1f}" cy="{y_at(value):.1f}" r="3" '
                f'fill="{colour}" />')
        if len(run) > 1:
            parts.append(
                f'<polyline fill="none" stroke="{colour}" stroke-width="2" '
                f'points="{" ".join(run)}" />')

        legend_y = top + 6 + index * 18
        parts.append(
            f'<rect x="{left + plot_w + 16}" y="{legend_y - 8}" width="10" '
            f'height="10" fill="{colour}" />')
        parts.append(
            f'<text x="{left + plot_w + 32}" y="{legend_y + 1}" font-size="11" '
            f'fill="currentColor">{_esc(entry.label[:24])}</text>')

    parts.append("</svg>")
    return "".join(parts)


def _esc(text: str) -> str:
    return (text.replace("&", "&amp;").replace("<", "&lt;")
            .replace(">", "&gt;").replace('"', "&quot;"))


def _write_html(path: str, title: str, periods: list[str],
                series: list[Series], unit: str, caveats: list[str]) -> None:
    """Write a standalone chart page — no CDN, so it works offline."""
    notes = "".join(f"<li>{_esc(c)}</li>" for c in caveats)
    rows = "".join(
        "<tr><th scope=\"row\">" + _esc(s.label) + "</th>"
        + "".join(
            "<td>" + ("—" if v is None else f"{v:.2f}") + "</td>"
            for v in s.values)
        + "</tr>"
        for s in series)
    head = "".join(f"<th>{_esc(p[:7])}</th>" for p in periods)

    document = f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{_esc(title)}</title>
<style>
  :root {{ color-scheme: light dark; --fg:#111; --bg:#fff; --mut:#666;
           --line:#e5e5e5; }}
  @media (prefers-color-scheme: dark) {{
    :root {{ --fg:#e8e8e8; --bg:#141414; --mut:#a0a0a0; --line:#333; }} }}
  body {{ margin:0; padding:2rem; background:var(--bg); color:var(--fg);
          font:14px/1.5 system-ui,-apple-system,Segoe UI,sans-serif; }}
  main {{ max-width:1000px; margin:0 auto; }}
  h1 {{ font-size:1.3rem; margin:0 0 .25rem; }}
  p.sub {{ color:var(--mut); margin:0 0 1.5rem; }}
  .chart {{ overflow-x:auto; border:1px solid var(--line); border-radius:8px;
            padding:1rem; margin-bottom:2rem; }}
  .scroll {{ overflow-x:auto; }}
  table {{ border-collapse:collapse; width:100%; font-variant-numeric:tabular-nums; }}
  th,td {{ padding:.4rem .6rem; border-bottom:1px solid var(--line);
           text-align:right; white-space:nowrap; }}
  th[scope=row] {{ text-align:left; font-weight:500; }}
  thead th {{ color:var(--mut); font-weight:500; }}
  ul.caveats {{ color:var(--mut); font-size:.85rem; margin-top:2rem;
                padding-left:1.2rem; }}
</style></head>
<body><main>
<h1>{_esc(title)}</h1>
<p class="sub">Values in {_esc(unit or 'units')}. Source: SEC EDGAR Form 13F-HR.</p>
<div class="chart">{_svg_chart(title, periods, series, unit)}</div>
<div class="scroll"><table>
<thead><tr><th scope="col">Position</th>{head}</tr></thead>
<tbody>{rows}</tbody>
</table></div>
<ul class="caveats">{notes}
<li>Informational only; not investment advice. Verify against the filing.</li>
</ul>
</main></body></html>
"""
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(document)
    print(f"\nwrote {path} — open it in a browser")


def _recent_quarters(cik: str, count: int) -> list[Filing]:
    """The newest `count` quarters for a manager, oldest first."""
    by_period = latest_per_period(list_filings(cik))
    periods = sorted(by_period, reverse=True)[:count]
    return [by_period[p] for p in sorted(periods)]


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def cmd_roster(args: argparse.Namespace) -> None:
    tiers = args.tier.upper().split(",") if args.tier else list(TIER_LABELS)
    for tier in tiers:
        funds = [f for f in ROSTER if f.tier == tier]
        if not funds:
            continue
        _header(f"Tier {tier} — {TIER_LABELS.get(tier, '')}")
        _table(
            ["Slug", "CIK", "Manager", "Note"],
            [[f.slug, f.cik, f.name, f.note] for f in funds],
            "llll",
        )
    print()
    print(f"{len(ROSTER)} managers, from {ROSTER_SOURCE}.")
    print("Any CIK also works in place of a slug, roster or not.")


def cmd_filings(args: argparse.Namespace) -> None:
    fund = resolve_fund(args.fund)
    filings = list_filings(fund.cik, all_history=args.all_history)
    if not filings:
        raise SystemExit(
            f"No 13F-HR filings found for {fund.name} ({fund.cik}).")

    _header(f"{fund.name} ({fund.cik}) — 13F-HR filings")
    rows = [
        [f.period, f.filed, f.form, f.accession,
            "thousands" if not f.values_in_dollars else "dollars"]
        for f in filings[:args.count]
    ]
    _table(["Period", "Filed", "Form", "Accession", "Value units"], rows, "lllll")

    amendments = [f for f in filings[:args.count] if f.is_amendment]
    if amendments:
        print()
        print(f"{len(amendments)} amendment(s) in range — these supersede the "
              "original and are what `holdings`/`deltas` use.")
    _disclaimer()


def _select_filing(filings: list[Filing], period: str | None, index: int) -> Filing:
    by_period = latest_per_period(filings)
    periods = sorted(by_period, reverse=True)
    if period:
        if period not in by_period:
            raise SystemExit(
                f"No filing for period {period}. Available: {', '.join(periods[:8])}"
            )
        return by_period[period]
    if index >= len(periods):
        raise SystemExit(f"Only {len(periods)} periods available.")
    return by_period[periods[index]]


def cmd_holdings(args: argparse.Namespace) -> None:
    fund = resolve_fund(args.fund)
    filings = list_filings(fund.cik)
    if not filings:
        raise SystemExit(
            f"No 13F-HR filings found for {fund.name} ({fund.cik}).")

    filing = _select_filing(filings, args.period, args.index)
    positions, total = load_quarter(filing)
    if not positions:
        raise SystemExit(
            f"Could not parse an information table for {filing.accession}.")

    ranked = sorted(positions.values(), key=lambda p: p.value, reverse=True)
    _header(f"{fund.name} — holdings as of {filing.period}")
    print(f"Filed {filing.filed} ({filing.form}), accession {filing.accession}")
    print(f"13F AUM: {_money(total)} across {len(ranked)} positions")
    print()

    rows: list[list[str]] = []
    for rank, position in enumerate(ranked[:args.top], start=1):
        weight = 100.0 * position.value / total if total else 0.0
        rows.append([
            str(rank),
            position.issuer[:34],
            position.cusip,
            position.put_call or "-",
            _money(position.value),
            f"{weight:.2f}%",
            _shares(position.shares),
            position.share_type,
        ])
    _table(
        ["#", "Issuer", "CUSIP", "Opt", "Value", "% AUM", "Shares", "Type"],
        rows,
        "llllrrrl",
    )

    top10 = sum(p.value for p in ranked[:10])
    if total:
        print()
        print(f"Top 10 concentration: {100.0 * top10 / total:.1f}% of 13F AUM")
    if any(p.put_call for p in ranked):
        print("Option rows are reported at notional and overstate economic size.")
    _disclaimer()


def cmd_deltas(args: argparse.Namespace) -> None:
    fund = resolve_fund(args.fund)
    filings = list_filings(fund.cik)
    if not filings:
        raise SystemExit(
            f"No 13F-HR filings found for {fund.name} ({fund.cik}).")

    by_period = latest_per_period(filings)
    periods = sorted(by_period, reverse=True)
    if len(periods) < 2:
        raise SystemExit("Need at least two quarters to compute deltas.")

    if args.to_period:
        curr = _select_filing(filings, args.to_period, 0)
    elif args.index >= len(periods):
        raise SystemExit(f"Only {len(periods)} periods available.")
    else:
        curr = by_period[periods[args.index]]
    if args.from_period:
        prev = _select_filing(filings, args.from_period, 0)
    else:
        older = [p for p in periods if p < curr.period]
        if not older:
            raise SystemExit(
                f"No quarter earlier than {curr.period} available.")
        prev = by_period[older[0]]

    curr_pos, curr_total = load_quarter(curr)
    prev_pos, prev_total = load_quarter(prev)
    if not curr_pos or not prev_pos:
        raise SystemExit(
            "Could not parse an information table for one of the quarters.")

    _header(f"{fund.name} — {prev.period} to {curr.period}")
    aum = f"13F AUM: {_money(prev_total)} -> {_money(curr_total)}"
    if prev_total:
        change = 100.0 * (curr_total - prev_total) / prev_total
        aum += f" ({change:+.1f}%)"
    print(aum)
    print(f"Positions: {len(prev_pos)} -> {len(curr_pos)}")
    print()

    scored: list[tuple[float, list[str]]] = []
    for key in set(curr_pos) | set(prev_pos):
        now = curr_pos.get(key)
        before = prev_pos.get(key)
        curr_weight = 100.0 * now.value / curr_total if now and curr_total else 0.0
        prev_weight = 100.0 * before.value / prev_total if before and prev_total else 0.0
        curr_shares = now.shares if now else 0.0
        prev_shares = before.shares if before else 0.0

        if before is None:
            action = "NEW"
        elif now is None:
            action = "EXIT"
        elif curr_shares > prev_shares:
            action = "ADD"
        elif curr_shares < prev_shares:
            action = "TRIM"
        else:
            action = "HOLD"

        if action == "HOLD" and not args.include_holds:
            continue

        template = now or before
        if template is None:
            continue
        if prev_shares:
            share_change = f"{100.0 * (curr_shares - prev_shares) / prev_shares:+.1f}%"
        else:
            share_change = "new"

        scored.append((abs(curr_weight - prev_weight), [
            action,
            template.issuer[:32],
            template.cusip,
            template.put_call or "-",
            f"{prev_weight:.2f}%",
            f"{curr_weight:.2f}%",
            f"{curr_weight - prev_weight:+.2f}",
            share_change,
            _money(now.value if now else 0.0),
        ]))

    scored.sort(key=lambda item: item[0], reverse=True)
    _table(
        ["Action", "Issuer", "CUSIP", "Opt", f"%AUM {prev.period[:7]}",
         f"%AUM {curr.period[:7]}", "Chg pp", "Shares", "Value now"],
        [row for _, row in scored[:args.top]],
        "llllrrrrr",
    )

    if curr.is_amendment or prev.is_amendment:
        print()
        print("One or both quarters use an amended filing (13F-HR/A).")
    print()
    print("Sorted by change in portfolio weight (pp), which is the "
          "signal; dollar changes conflate sizing with market moves.")
    print("Action reflects the share count, which is the manager's "
          "decision; weight also moves with price, so a TRIM can still "
          "gain weight.")
    print("Share changes are not split-adjusted — verify any move above "
          "50% against the issuer's corporate actions.")
    _disclaimer()


def cmd_consensus(args: argparse.Namespace) -> None:
    if args.funds:
        funds = [resolve_fund(f) for f in args.funds.split(",")]
    elif args.tier:
        tiers = args.tier.upper().split(",")
        funds = [f for f in ROSTER if f.tier in tiers]
    else:
        funds = list(ROSTER)
    if not funds:
        raise SystemExit("No managers selected.")

    curr_holders: dict[str, set[str]] = {}
    prev_holders: dict[str, set[str]] = {}
    curr_value: dict[str, float] = {}
    names: dict[str, str] = {}
    skipped: list[str] = []

    for fund in funds:
        print(f"fetching {fund.slug} ...", file=sys.stderr)
        try:
            filings = list_filings(fund.cik)
            if not filings:
                skipped.append(f"{fund.slug} (no filings)")
                continue
            filing = _select_filing(filings, args.period, args.index)
            positions, _ = load_quarter(filing)
            if not positions:
                skipped.append(f"{fund.slug} (unparsed table)")
                continue
        except SystemExit as exc:
            skipped.append(f"{fund.slug} ({exc})")
            continue

        for position in positions.values():
            if position.put_call:
                continue
            curr_holders.setdefault(position.cusip, set()).add(fund.slug)
            curr_value[position.cusip] = curr_value.get(
                position.cusip, 0.0) + position.value
            names.setdefault(position.cusip, position.issuer)

        if not args.compare:
            continue

        by_period = latest_per_period(filings)
        older = [p for p in sorted(
            by_period, reverse=True) if p < filing.period]
        if not older:
            continue
        prior, _ = load_quarter(by_period[older[0]])
        for position in prior.values():
            if position.put_call:
                continue
            prev_holders.setdefault(position.cusip, set()).add(fund.slug)
            names.setdefault(position.cusip, position.issuer)

    _header(f"Consensus across {len(funds) - len(skipped)} managers")
    scored: list[tuple[int, float, list[str]]] = []
    for cusip, holders in curr_holders.items():
        before = len(prev_holders.get(cusip, set()))
        value = curr_value.get(cusip, 0.0)
        scored.append((len(holders), value, [
            str(len(holders)),
            f"{len(holders) - before:+d}" if args.compare else "-",
            names.get(cusip, "")[:34],
            cusip,
            _money(value),
            ",".join(sorted(holders)[:5]) +
            ("..." if len(holders) > 5 else ""),
        ]))

    scored.sort(key=lambda item: (item[0], item[1]), reverse=True)
    _table(
        ["Held", "Chg", "Issuer", "CUSIP", "Aggregate value", "Managers"],
        [row for _, _, row in scored[:args.top]],
        "rrllrl",
    )

    if skipped:
        print()
        print(f"Skipped: {', '.join(skipped)}")
    print()
    print("Common stock only — option rows excluded. Aggregate value sums "
          "managers of very different size, so read the holder count first.")
    _disclaimer()


def cmd_history(args: argparse.Namespace) -> None:
    fund = resolve_fund(args.fund)
    quarters = _recent_quarters(fund.cik, args.quarters)
    if len(quarters) < 2:
        raise SystemExit(f"Need at least two quarters; found {len(quarters)}.")

    periods = [f.period for f in quarters]
    weights: list[dict[str, float]] = []
    labels: dict[str, str] = {}
    totals: list[float] = []

    for filing in quarters:
        print(f"fetching {filing.period} ...", file=sys.stderr)
        positions, total = load_quarter(filing)
        totals.append(total)
        quarter: dict[str, float] = {}
        for position in positions.values():
            if position.put_call and not args.include_options:
                continue
            labels.setdefault(position.cusip, position.issuer)
            share = 100.0 * position.value / total if total else 0.0
            quarter[position.cusip] = quarter.get(position.cusip, 0.0) + share
        weights.append(quarter)

    series = [
        Series(labels[cusip], cusip, [q.get(cusip) for q in weights])
        for cusip in labels
    ]
    series.sort(key=lambda s: (s.final or 0.0, s.peak), reverse=True)
    shown = series[:args.top]
    ceiling = max([s.peak for s in shown] or [0.0])

    _header(f"{fund.name} — position weight over {len(periods)} quarters")
    print(f"{periods[0]} to {periods[-1]}, % of 13F AUM")
    print(f"13F AUM: {_money(totals[0])} -> {_money(totals[-1])}")
    print()

    rows = [
        [
            s.label[:30],
            s.key,
            _sparkline(s.values, ceiling),
            "—" if s.first is None else f"{s.first:.2f}%",
            "exited" if s.final is None else f"{s.final:.2f}%",
            f"{(s.final or 0.0) - (s.first or 0.0):+.2f}",
        ]
        for s in shown
    ]
    _table(
        ["Issuer", "CUSIP", f"{periods[0][:7]} -> {periods[-1][:7]}",
         "First held", "Latest", "Chg pp"],
        rows,
        "lllrrr",
    )
    print()
    print("Sparkline is scaled across all rows shown, so heights compare; "
          "'.' marks a quarter the position was not held.")

    _export(args, f"{fund.name} — position weight (% of 13F AUM)",
            periods, shown, "%",
            ["Weights are % of that quarter's 13F AUM, not dollars.",
             "A gap means the manager did not report the position that quarter.",
             "13F is long-only and up to 45 days stale."])
    _disclaimer()


def cmd_crowding(args: argparse.Namespace) -> None:
    if args.funds:
        funds = [resolve_fund(f) for f in args.funds.split(",")]
    elif args.tier:
        tiers = args.tier.upper().split(",")
        funds = [f for f in ROSTER if f.tier in tiers]
    else:
        funds = list(ROSTER)

    needle = args.cusip.upper() if args.cusip else ""
    issuer_query = args.issuer.upper() if args.issuer else ""
    if not needle and not issuer_query:
        raise SystemExit("Pass --cusip or --issuer to pick a security.")

    matched_label = issuer_query or needle

    # Managers file on different schedules, so pin a single calendar first
    # and then ask each manager about exactly those quarters. Unioning each
    # manager's own newest N periods instead would stretch the axis back to
    # a quarter almost nobody was sampled in, which reads as "nobody held
    # it" when it means "almost nobody was measured".
    calendars: dict[str, dict[str, Filing]] = {}
    for fund in funds:
        print(f"listing {fund.slug} ...", file=sys.stderr)
        try:
            calendars[fund.slug] = latest_per_period(list_filings(fund.cik))
        except SystemExit as exc:
            print(f"  skipped {fund.slug}: {exc}", file=sys.stderr)

    every_period = {p for cal in calendars.values() for p in cal}
    periods = sorted(every_period)[-args.quarters:]
    if not periods:
        raise SystemExit("No filings found for the selected managers.")

    per_fund: dict[str, dict[str, float]] = {}
    filed: dict[str, set[str]] = {}

    for slug, calendar in calendars.items():
        print(f"fetching {slug} ...", file=sys.stderr)
        for period in periods:
            filing = calendar.get(period)
            if filing is None:
                continue
            filed.setdefault(slug, set()).add(period)
            positions, total = load_quarter(filing)
            for position in positions.values():
                if position.put_call:
                    continue
                hit = (position.cusip == needle if needle
                       else issuer_query in position.issuer.upper())
                if not hit:
                    continue
                matched_label = position.issuer
                share = 100.0 * position.value / total if total else 0.0
                per_fund.setdefault(slug, {})[period] = share

    if not per_fund:
        raise SystemExit(f"No roster manager reported {matched_label}.")

    series = [
        Series(
            slug,
            slug,
            [per_fund[slug].get(p) for p in periods],
            [p in filed.get(slug, set()) for p in periods],
        )
        for slug in per_fund
    ]
    series.sort(key=lambda s: (s.final or 0.0, s.peak), reverse=True)

    _header(f"{matched_label} — {len(series)} of {len(funds)} managers, "
            f"{len(periods)} quarters")
    rows: list[list[str]] = []
    for i, period in enumerate(periods):
        holders = [s.label for s in series if s.values[i] is not None]
        reporting = sum(
            1 for slug in calendars if period in filed.get(slug, set()))
        rows.append([
            period,
            str(len(holders)),
            str(reporting),
            ", ".join(holders) or "—",
        ])
    _table(["Period", "Holders", "Filed", "Held by"], rows, "lrrl")
    print()
    print("'Filed' is how many selected managers had filed for that quarter — "
          "a low count means thin coverage, not a sell-off.")
    print()

    ceiling = max([s.peak for s in series] or [0.0])
    _table(
        ["Manager", f"{periods[0][:7]} -> {periods[-1][:7]}",
         "First held", "Latest", "Chg pp"],
        [
            [
                s.label,
                _sparkline(s.values, ceiling, s.covered),
                "—" if s.first is None else f"{s.first:.2f}%",
                _final_cell(s),
                f"{(s.final or 0.0) - (s.first or 0.0):+.2f}",
            ]
            for s in series
        ],
        "llrrr",
    )
    print()
    print("Holder count is the crowding signal; per-manager weight shows "
          "who is driving it. '?' marks a quarter the manager had not filed.")

    _export(args, f"{matched_label} — weight by manager (% of 13F AUM)",
            periods, series, "%",
            ["Each line is one manager's weight in this security.",
             ("Managers file on their own schedule, so a trailing gap may "
              "mean 'not filed yet' rather than 'sold'."),
             "Common stock only; option rows are excluded."])
    _disclaimer()


def _export(args: argparse.Namespace, title: str, periods: list[str],
            series: list[Series], unit: str, caveats: list[str]) -> None:
    """Honour --csv / --html for the trend commands."""
    if getattr(args, "csv", None):
        _write_csv(
            args.csv,
            ["label", "key"] + periods,
            [[s.label, s.key] + ["" if v is None else f"{v:.4f}"
                                 for v in s.values] for s in series],
        )
    if getattr(args, "html", None):
        _write_html(args.html, title, periods, series, unit, caveats)


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="13f.py",
        description="13F-HR holdings and quarter-over-quarter deltas from SEC EDGAR",
    )
    sub = parser.add_subparsers(dest="command", required=True)

    r = sub.add_parser("roster",
                       help="List the watchlist of 13F filers")
    r.add_argument("--tier",
                   default=None,
                   help="Only show these tiers, comma-separated (A,B,C,D,E)")
    r.set_defaults(func=cmd_roster)

    f = sub.add_parser("filings",
                       help="List 13F-HR filings for a manager")
    f.add_argument("fund",
                   help="Roster slug or CIK")
    f.add_argument("--count",
                   type=int,
                   default=12,
                   help="Max filings to show (default 12)")
    f.add_argument("--all-history",
                   action="store_true",
                   dest="all_history",
                   help="Also fetch EDGAR's older submission pages (slower)")
    f.set_defaults(func=cmd_filings)

    h = sub.add_parser("holdings",
                       help="Show one quarter's positions, weighted by %% of 13F AUM")
    h.add_argument("fund",
                   help="Roster slug or CIK")
    h.add_argument("--period",
                   default=None,
                   help="Quarter end date YYYY-MM-DD (default: most recent)")
    h.add_argument("--index",
                   type=int,
                   default=0,
                   help="Which quarter back from the latest (0=latest, default 0)")
    h.add_argument("--top",
                   type=int,
                   default=25,
                   help="Rows to display (default 25)")
    h.set_defaults(func=cmd_holdings)

    d = sub.add_parser("deltas",
                       help="Quarter-over-quarter position changes for one manager")
    d.add_argument("fund",
                   help="Roster slug or CIK")
    d.add_argument("--from",
                   default=None,
                   dest="from_period",
                   help="Earlier quarter end YYYY-MM-DD (default: quarter before --to)")
    d.add_argument("--to",
                   default=None,
                   dest="to_period",
                   help="Later quarter end YYYY-MM-DD (default: most recent)")
    d.add_argument("--index",
                   type=int,
                   default=0,
                   help="Which quarter back to use as the later one (default 0)")
    d.add_argument("--top",
                   type=int,
                   default=30,
                   help="Rows to display (default 30)")
    d.add_argument("--include-holds",
                   action="store_true",
                   dest="include_holds",
                   help="Also show unchanged positions")
    d.set_defaults(func=cmd_deltas)

    c = sub.add_parser("consensus",
                       help="Aggregate holdings across managers (crowding / holder counts)")
    c.add_argument("--funds",
                   default=None,
                   help="Comma-separated slugs or CIKs (default: whole roster)")
    c.add_argument("--tier",
                   default=None,
                   help="Restrict to these roster tiers, comma-separated")
    c.add_argument("--period",
                   default=None,
                   help="Quarter end date YYYY-MM-DD (default: each manager's most recent)")
    c.add_argument("--index",
                   type=int,
                   default=0,
                   help="Which quarter back from each manager's latest (default 0)")
    c.add_argument("--top",
                   type=int,
                   default=40,
                   help="Rows to display (default 40)")
    c.add_argument("--compare",
                   action="store_true",
                   help="Also compute the change in holder count vs the prior quarter")
    c.set_defaults(func=cmd_consensus)

    hi = sub.add_parser("history",
                        help="One manager's position weights across N quarters")
    hi.add_argument("fund",
                    help="Roster slug or CIK")
    hi.add_argument("--quarters",
                    type=int,
                    default=8,
                    help="How many quarters back (default 8)")
    hi.add_argument("--top",
                    type=int,
                    default=15,
                    help="Rows to display (default 15)")
    hi.add_argument("--include-options",
                    action="store_true",
                    dest="include_options",
                    help="Include option positions, which are reported at notional")
    hi.add_argument("--csv",
                    default=None,
                    metavar="PATH",
                    help="Also write the series to a CSV for a spreadsheet")
    hi.add_argument("--html",
                    default=None,
                    metavar="PATH",
                    help="Also write a standalone chart page (no network needed)")
    hi.set_defaults(func=cmd_history)

    cr = sub.add_parser("crowding",
                        help="One security's holder count and weights across managers over time")
    cr.add_argument("--cusip",
                    default=None,
                    help="Security CUSIP, e.g. 02079K107")
    cr.add_argument("--issuer",
                    default=None,
                    help="Case-insensitive substring of the issuer name instead of a CUSIP")
    cr.add_argument("--funds",
                    default=None,
                    help="Comma-separated slugs or CIKs (default: whole roster)")
    cr.add_argument("--tier",
                    default=None,
                    help="Restrict to these roster tiers, comma-separated")
    cr.add_argument("--quarters",
                    type=int,
                    default=6,
                    help="How many quarters back (default 6)")
    cr.add_argument("--csv",
                    default=None,
                    metavar="PATH",
                    help="Also write the series to a CSV for a spreadsheet")
    cr.add_argument("--html",
                    default=None,
                    metavar="PATH",
                    help="Also write a standalone chart page (no network needed)")
    cr.set_defaults(func=cmd_crowding)

    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        args.func(args)
    except KeyboardInterrupt:
        return 130
    return 0


if __name__ == "__main__":
    sys.exit(main())
