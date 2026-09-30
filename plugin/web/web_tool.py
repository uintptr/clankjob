#!/usr/bin/env python3
"""clankjob web plugin: search the web, read a page as text, and watch pages and feeds.

A command plugin (design section 9.9), standard library only:

    search --query=Q [--count N] [--start N] [--country CC] [--only-country CC]
           [--language LL] [--interface-language LL] [--since d7|w2|m6|y1]
           [--site S | --exclude-site S] [--file-type EXT] [--exact T] [--exclude T]
           [--sort-by-date] [--safe]
                                      web search through Google's Custom Search JSON API
                                      (GOOGLE_CSE_API_KEY, GOOGLE_CSE_ID; defaults for
                                      country and languages: GOOGLE_CSE_GL, _HL, _LR)
    page --url=U [--links]            a page's readable text (title, headings, lists)
    check-changed                     wait conditions: stdin {"params", "cursor"},
    check-contains                    stdout {"status", "events", "cursor"}
    check-feed

Pages are fetched from the server itself, so addresses of this machine (loopback,
link-local) are refused, at every redirect too; the local network is allowed.
"""

import argparse
import dataclasses
import hashlib
import ipaddress
import json
import os
import re
import socket
import sys
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass, field
from html.parser import HTMLParser
from xml.etree import ElementTree

Json = dict[str, object]

TIMEOUT = 30
MAX_BYTES = 5_000_000
# Google returns at most 10 results per call and 100 per query.
MAX_COUNT = 10
MAX_START = 91
DEFAULT_COUNT = 8
GOOGLE_URL = "https://www.googleapis.com/customsearch/v1"
# What a watch remembers of a page, to say what changed.
MAX_CURSOR_TEXT = 100_000
MAX_CHANGED_LINES = 20
AROUND_LINES = 5
USER_AGENT = "Mozilla/5.0 (X11; Linux x86_64; rv:128.0) Gecko/20100101 Firefox/128.0 clankjob-web/0.1"

SKIPPED = {"script", "style", "noscript", "svg", "template", "head", "iframe", "canvas"}
BLOCKS = {"p", "div", "section", "article", "main", "header", "footer", "aside", "nav", "form", "table", "tr",
          "ul", "ol", "dl", "dt", "dd", "blockquote", "pre", "figure", "figcaption", "br", "hr", "h1", "h2", "h3",
          "h4", "h5", "h6", "li", "td", "th", "details", "summary"}
HEADINGS = {"h1": "# ", "h2": "## ", "h3": "### ", "h4": "#### ", "h5": "##### ", "h6": "###### "}


class ToolError(Exception):
    """A failure reported to the LLM (printed to stderr, exit status 1)."""


# ---------------------------------------------------------------- fetching


def check_host(host: str) -> None:
    """Refuse hosts that resolve to this machine: loopback, link-local, unspecified."""
    try:
        infos = socket.getaddrinfo(host, None)
    except OSError as error:
        raise ToolError(f"cannot resolve {host}: {error}") from None
    for info in infos:
        address = ipaddress.ip_address(str(info[4][0]).split("%")[0])
        if address.is_loopback or address.is_link_local or address.is_unspecified or address.is_multicast:
            raise ToolError(f"{host} is an address of this server ({address}); it cannot be fetched")


def check_url(url: str) -> str:
    parts = urllib.parse.urlsplit(url.strip())
    if parts.scheme not in ("http", "https") or not parts.hostname:
        raise ToolError(f"only http and https URLs can be fetched, not {url!r}")
    check_host(parts.hostname)
    return urllib.parse.urlunsplit(parts)


class CheckedRedirects(urllib.request.HTTPRedirectHandler):
    """Follow redirects only to URLs that pass the same checks."""

    def redirect_request(self, req: urllib.request.Request, fp: object, code: int, msg: str,
                         headers: object, newurl: str) -> urllib.request.Request | None:
        return super().redirect_request(req, fp, code, msg, headers, check_url(newurl))  # pyright: ignore[reportArgumentType]


OPENER = urllib.request.build_opener(CheckedRedirects)


@dataclass(frozen=True)
class Fetched:
    url: str
    content_type: str
    body: bytes

    @property
    def text(self) -> str:
        found = re.search(r"charset=([\w-]+)", self.content_type)
        try:
            return self.body.decode(found.group(1) if found else "utf-8", errors="replace")
        except LookupError:
            return self.body.decode("utf-8", errors="replace")


def fetch(url: str, headers: dict[str, str] | None = None) -> Fetched:
    request = urllib.request.Request(check_url(url), headers={"User-Agent": USER_AGENT, "Accept-Language": "en,fr;q=0.8",
                                                              **(headers or {})})
    try:
        with OPENER.open(request, timeout=TIMEOUT) as response:
            body = response.read(MAX_BYTES + 1)
            final, content_type = response.geturl(), response.headers.get("Content-Type", "")
    except urllib.error.HTTPError as error:
        raise ToolError(f"{url} answered HTTP {error.code} {error.reason}") from None
    except (OSError, ValueError) as error:
        raise ToolError(f"cannot fetch {url}: {error}") from None
    if len(body) > MAX_BYTES:
        raise ToolError(f"{url} is larger than {MAX_BYTES // 1_000_000} MB")
    return Fetched(final, content_type.lower(), body)


# ---------------------------------------------------------------- HTML to text


class TextExtractor(HTMLParser):
    """Readable text of a page: headings, paragraphs and list items on their own lines,
    scripts, styles and other invisible parts left out; links collected."""

    def __init__(self, base: str) -> None:
        super().__init__(convert_charrefs=True)
        self.base = base
        self.title = ""
        self.lines: list[str] = []
        self.current: list[str] = []
        self.links: list[tuple[str, str]] = []
        self.skipping = 0
        self.in_title = False
        self.href: str | None = None
        self.link_text: list[str] = []

    def flush(self, prefix: str = "") -> None:
        text = " ".join("".join(self.current).split())
        if text:
            self.lines.append(prefix + text)
        self.current = []

    def handle_starttag(self, tag: str, attrs: list[tuple[str, str | None]]) -> None:
        if tag in SKIPPED:
            self.skipping += 1
        elif tag == "title":
            self.in_title = True
        if self.skipping:
            return
        if tag in BLOCKS:
            self.flush()
        if tag == "li":
            self.current.append("- ")
        if tag == "a":
            href = dict(attrs).get("href")
            self.href = urllib.parse.urljoin(self.base, href) if href else None
            self.link_text = []

    def handle_endtag(self, tag: str) -> None:
        if tag in SKIPPED:
            self.skipping = max(0, self.skipping - 1)
        elif tag == "title":
            self.in_title = False
        if self.skipping:
            return
        if tag in HEADINGS:
            self.flush(HEADINGS[tag])
        elif tag in BLOCKS:
            self.flush()
        if tag == "a" and self.href:
            text = " ".join("".join(self.link_text).split())
            if text and self.href.startswith(("http://", "https://")):
                self.links.append((text, self.href))
            self.href = None

    def handle_data(self, data: str) -> None:
        if self.in_title:
            self.title += data
        if self.skipping:
            return
        self.current.append(data)
        if self.href:
            self.link_text.append(data)


@dataclass(frozen=True)
class Page:
    url: str
    title: str
    lines: list[str]
    links: list[tuple[str, str]] = field(default_factory=list)


def read_page(url: str) -> Page:
    fetched = fetch(url)
    if "html" in fetched.content_type or fetched.text.lstrip()[:200].lower().startswith(("<!doctype html", "<html")):
        parser = TextExtractor(fetched.url)
        parser.feed(fetched.text)
        parser.close()
        parser.flush()
        return Page(fetched.url, " ".join(parser.title.split()), parser.lines, parser.links)
    if fetched.content_type.startswith(("text/", "application/json", "application/xml")) or not fetched.content_type:
        return Page(fetched.url, "", [line.rstrip() for line in fetched.text.splitlines() if line.strip()])
    raise ToolError(f"{url} is {fetched.content_type.split(';')[0]}, not a web page: download it with run_command "
                    "(curl) or ask the owner to add it to the case")


def page_text(url: str, links: bool) -> str:
    page = read_page(url)
    parts = [f"{page.title}\n{page.url}" if page.title else page.url, "\n".join(page.lines) or "(no text)"]
    if links and page.links:
        unique = list(dict.fromkeys(page.links))
        parts.append("Links:\n" + "\n".join(f"- {text}: {href}" for text, href in unique))
    return "\n\n".join(parts)


# ---------------------------------------------------------------- search


@dataclass(frozen=True)
class Google:
    """Google's Custom Search JSON API: an API key and a Programmable Search Engine id."""

    key: str
    cx: str

    @classmethod
    def from_env(cls, env: dict[str, str]) -> "Google":
        key, cx = env.get("GOOGLE_CSE_API_KEY", "").strip(), env.get("GOOGLE_CSE_ID", "").strip()
        if not key or not cx:
            raise ToolError("web search is not configured: set GOOGLE_CSE_API_KEY and GOOGLE_CSE_ID in the web "
                            "plugin's config.toml [env]")
        return cls(key, cx)

    def url(self, query: str, count: int, start: int, options: dict[str, str]) -> str:
        params = {"key": self.key, "cx": self.cx, "q": query, "num": str(count), "start": str(start), **options}
        return f"{GOOGLE_URL}?{urllib.parse.urlencode(params)}"

    def scrub(self, text: str) -> str:
        return text.replace(self.key, "***")


def google_error(body: bytes) -> str:
    """The message of a Google API error body ({"error": {"code", "message"}})."""
    try:
        error = json.loads(body).get("error", {})
    except (ValueError, AttributeError):
        return ""
    return str(error.get("message", "")) if isinstance(error, dict) else ""


def google_results(reply: object) -> list[Json]:
    items = reply.get("items", []) if isinstance(reply, dict) else []
    return [{"title": str(item.get("title", "")), "url": str(item.get("link", "")),
             "snippet": " ".join(str(item.get("snippet", "")).split())}
            for item in items if isinstance(item, dict) and item.get("link")]


@dataclass(frozen=True)
class SearchOptions:
    """What narrows or localizes a search, in the tool's terms; `google()` turns it into
    Custom Search parameters. Empty fields are left out."""

    country: str = ""  # gl: boost results from this country (two letters)
    only_country: str = ""  # cr: only results from this country
    language: str = ""  # lr: only results in this language
    interface_language: str = ""  # hl
    since: str = ""  # dateRestrict: d7, w2, m6, y1
    site: str = ""  # siteSearch, included
    exclude_site: str = ""  # siteSearch, excluded
    file_type: str = ""  # fileType: pdf, docx…
    exact: str = ""  # exactTerms
    exclude: str = ""  # excludeTerms
    sort_by_date: bool = False  # sort=date
    safe: bool = False  # safe=active

    @classmethod
    def with_defaults(cls, env: dict[str, str], **given: str | bool) -> "SearchOptions":
        """The call's options, over the configured defaults (GOOGLE_CSE_GL, _HL, _LR)."""
        base = cls(country=env.get("GOOGLE_CSE_GL", "").strip(), language=env.get("GOOGLE_CSE_LR", "").strip(),
                   interface_language=env.get("GOOGLE_CSE_HL", "").strip())
        return dataclasses.replace(base, **{name: value for name, value in given.items() if value not in ("", False)})

    def google(self) -> dict[str, str]:
        def code(value: str, what: str) -> str:
            if not re.fullmatch(r"[A-Za-z]{2}", value):
                raise ToolError(f"{what} must be a two-letter code like `ca` or `fr`, got {value!r}")
            return value.lower()

        params: dict[str, str] = {}
        if self.country:
            params["gl"] = code(self.country, "country")
        if self.only_country:
            params["cr"] = f"country{code(self.only_country, 'only_country').upper()}"
        if self.language:
            params["lr"] = f"lang_{code(self.language, 'language')}"
        if self.interface_language:
            params["hl"] = code(self.interface_language, "interface_language")
        if self.since:
            if not re.fullmatch(r"[dwmy][1-9]\d{0,3}", self.since):
                raise ToolError(f"since must be like d7, w2, m6 or y1 (days, weeks, months, years), got {self.since!r}")
            params["dateRestrict"] = self.since
        if self.site and self.exclude_site:
            raise ToolError("give either site or exclude_site, not both")
        if self.site or self.exclude_site:
            params["siteSearch"] = self.site or self.exclude_site
            params["siteSearchFilter"] = "i" if self.site else "e"
        if self.file_type:
            if not re.fullmatch(r"[A-Za-z0-9]{1,8}", self.file_type.lstrip(".")):
                raise ToolError(f"file_type must be an extension like pdf, got {self.file_type!r}")
            params["fileType"] = self.file_type.lstrip(".").lower()
        if self.exact:
            params["exactTerms"] = self.exact
        if self.exclude:
            params["excludeTerms"] = self.exclude
        if self.sort_by_date:
            params["sort"] = "date"
        params["safe"] = "active" if self.safe else "off"
        return params


def search(env: dict[str, str], query: str, count: int, start: int = 1,
           options: SearchOptions | None = None) -> Json:
    if not query.strip():
        raise ToolError("query is empty")
    if not 1 <= count <= MAX_COUNT:
        raise ToolError(f"count must be 1 to {MAX_COUNT}")
    if not 1 <= start <= MAX_START:
        raise ToolError(f"start must be 1 to {MAX_START}")
    google = Google.from_env(env)
    params = (options or SearchOptions.with_defaults(env)).google()
    request = urllib.request.Request(google.url(query, count, start, params), headers={"Accept": "application/json"})
    try:
        with urllib.request.urlopen(request, timeout=TIMEOUT) as response:
            reply = json.loads(response.read())
    except urllib.error.HTTPError as error:
        detail = google_error(error.read()) or str(error)
        raise ToolError(f"Google search failed (HTTP {error.code}): {google.scrub(detail)}") from None
    except (OSError, ValueError) as error:
        raise ToolError(f"cannot reach Google search: {google.scrub(str(error))}") from None
    results = google_results(reply)
    if not results:
        raise ToolError(f"no results for {query!r}")
    total = reply.get("searchInformation", {}).get("totalResults") if isinstance(reply, dict) else None
    found: Json = {"query": query, "options": {name: value for name, value in params.items() if "safe" != name},
                   "results": results}
    if total:
        found["total"] = total
    if start + len(results) <= MAX_START:
        found["next_start"] = start + len(results)
    return found


# ---------------------------------------------------------------- wait conditions


def text_param(params: Json, name: str, required: bool = True) -> str:
    value = params.get(name)
    if isinstance(value, str) and value.strip():
        return value.strip()
    if required:
        raise ToolError(f"`{name}` is required")
    return ""


def around(lines: list[str], needle: str) -> list[str]:
    """The lines near each line containing `needle` (case-insensitive)."""
    wanted = needle.casefold()
    keep: set[int] = set()
    for index, line in enumerate(lines):
        if wanted in line.casefold():
            keep.update(range(max(0, index - AROUND_LINES), min(len(lines), index + AROUND_LINES + 1)))
    return [lines[index] for index in sorted(keep)]


def check_changed(params: Json, cursor: object) -> Json:
    """Fires when the page's text (or the part around `around`) differs from when the
    wait started. The first check only records it."""
    url = text_param(params, "url")
    near = text_param(params, "around", required=False)
    page = read_page(url)
    lines = around(page.lines, near) if near else page.lines
    text = "\n".join(lines)
    digest = hashlib.sha256(text.encode()).hexdigest()
    now = {"hash": digest, "text": text[:MAX_CURSOR_TEXT]}
    if not isinstance(cursor, dict) or "hash" not in cursor:
        return {"status": "pending", "events": [], "cursor": now}
    if cursor.get("hash") == digest:
        return {"status": "pending", "events": [], "cursor": cursor}
    before = str(cursor.get("text", "")).splitlines()
    old, new = set(before), set(lines)
    added = [line for line in lines if line not in old]
    removed = [line for line in before if line not in new]
    event: Json = {"url": page.url, "title": page.title, "added": added[:MAX_CHANGED_LINES],
                   "removed": removed[:MAX_CHANGED_LINES]}
    if len(added) > MAX_CHANGED_LINES or len(removed) > MAX_CHANGED_LINES:
        event["note"] = f"{len(added)} lines added and {len(removed)} removed; read the page with web_page"
    if near and not lines:
        event["note"] = f"{near!r} is no longer on the page"
    return {"status": "fired", "events": [event], "cursor": now}


def check_contains(params: Json, cursor: object) -> Json:
    """Fires as soon as the page contains `text` (or, with `absent`, no longer does),
    including on the first check."""
    url = text_param(params, "url")
    needle = text_param(params, "text")
    absent = params.get("absent") is True
    page = read_page(url)
    matching = [line for line in page.lines if needle.casefold() in line.casefold()]
    found = bool(matching) or needle.casefold() in page.title.casefold()
    if found == absent:
        return {"status": "pending", "events": [], "cursor": None}
    event: Json = {"url": page.url, "title": page.title, "text": needle, "found": found}
    if matching:
        event["lines"] = matching[:5]
    return {"status": "fired", "events": [event], "cursor": None}


def feed_items(fetched: Fetched) -> list[Json]:
    """Items of an RSS 2.0 or Atom feed: id, title, link, date."""
    try:
        root = ElementTree.fromstring(fetched.body)
    except ElementTree.ParseError as error:
        raise ToolError(f"{fetched.url} is not an RSS or Atom feed ({error})") from None
    atom = "{http://www.w3.org/2005/Atom}"
    items: list[Json] = []
    for entry in root.iter(f"{atom}entry"):
        link = entry.find(f"{atom}link[@rel='alternate']")
        link = link if link is not None else entry.find(f"{atom}link")
        items.append({"id": entry.findtext(f"{atom}id") or (link.get("href") if link is not None else ""),
                      "title": (entry.findtext(f"{atom}title") or "").strip(),
                      "link": link.get("href", "") if link is not None else "",
                      "date": entry.findtext(f"{atom}updated") or entry.findtext(f"{atom}published") or ""})
    for item in root.iter("item"):
        link = (item.findtext("link") or "").strip()
        items.append({"id": (item.findtext("guid") or link or item.findtext("title") or "").strip(),
                      "title": (item.findtext("title") or "").strip(), "link": link,
                      "date": (item.findtext("pubDate") or "").strip()})
    return [item for item in items if item["id"]]


def check_feed(params: Json, cursor: object) -> Json:
    """Fires when the feed has items it did not have when the wait started (optionally
    only those whose title contains `contains`)."""
    url = text_param(params, "url")
    wanted = text_param(params, "contains", required=False).casefold()
    items = feed_items(fetch(url))
    ids = [str(item["id"]) for item in items]
    if not isinstance(cursor, dict) or not isinstance(cursor.get("seen"), list):
        return {"status": "pending", "events": [], "cursor": {"seen": ids}}
    seen = set(map(str, cursor["seen"]))
    new = [item for item in items if str(item["id"]) not in seen and wanted in str(item["title"]).casefold()]
    # Remember everything seen so far, so an item that drops off and comes back is not new.
    remembered = {"seen": list(dict.fromkeys([*ids, *map(str, cursor["seen"])]))[:1000]}
    if not new:
        return {"status": "pending", "events": [], "cursor": remembered}
    return {"status": "fired", "events": [{"feed": url, "new_items": new[:MAX_CHANGED_LINES]}], "cursor": remembered}


CHECKS = {"check-changed": check_changed, "check-contains": check_contains, "check-feed": check_feed}


# ---------------------------------------------------------------- main


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    search_parser = sub.add_parser("search")
    search_parser.add_argument("--query", required=True)
    search_parser.add_argument("--count", type=int, default=DEFAULT_COUNT)
    search_parser.add_argument("--start", type=int, default=1)
    for name in ("country", "only-country", "language", "interface-language", "since", "site", "exclude-site",
                 "file-type", "exact", "exclude"):
        search_parser.add_argument(f"--{name}", default="")
    search_parser.add_argument("--sort-by-date", action="store_true")
    search_parser.add_argument("--safe", action="store_true")
    page_parser = sub.add_parser("page")
    page_parser.add_argument("--url", required=True)
    page_parser.add_argument("--links", action="store_true")
    for name in CHECKS:
        sub.add_parser(name)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        if "search" == args.command:
            print(json.dumps(search(dict(os.environ), args.query, args.count, args.start, SearchOptions.with_defaults(
                dict(os.environ), country=args.country, only_country=args.only_country, language=args.language,
                interface_language=args.interface_language, since=args.since, site=args.site,
                exclude_site=args.exclude_site, file_type=args.file_type, exact=args.exact, exclude=args.exclude,
                sort_by_date=args.sort_by_date, safe=args.safe)), ensure_ascii=False))
        elif "page" == args.command:
            print(page_text(args.url, args.links))
        else:
            request = json.loads(sys.stdin.read() or "{}")
            params = request.get("params") if isinstance(request, dict) else None
            cursor = request.get("cursor") if isinstance(request, dict) else None
            print(json.dumps(CHECKS[args.command](params if isinstance(params, dict) else {}, cursor),
                             ensure_ascii=False))
    except ToolError as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
