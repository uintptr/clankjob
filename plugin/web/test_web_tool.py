#!/usr/bin/env python3
"""Tests for the web plugin, offline (pages, feeds and Google are faked). Run: python3 -m unittest -v test_web_tool.py

WEB_LIVE_TEST=1 also runs check_config.py's checks against the real web (and Google, if configured)."""

import io
import json
import os
import unittest
import urllib.error
from email.message import Message
from pathlib import Path
from typing import ClassVar, Self
from unittest import mock

import web_tool as tool
from web_tool import Fetched, Json, Page, ToolError

HTML = b"""<!doctype html><html><head><title> Panel  shop </title><style>p{}</style></head>
<body><nav><a href="/cart">Cart</a></nav><script>var x = "hidden";</script>
<h1>200A panel</h1><p>Price: <b>$450</b></p><ul><li>In stock</li><li>Ships in 2 days</li></ul>
<p>See <a href="https://example.com/specs">the specs</a>.</p></body></html>"""
RSS = b"""<rss><channel><title>Shop</title>
<item><title>Panel back in stock</title><link>https://shop/a</link><guid>a</guid><pubDate>Mon</pubDate></item>
<item><title>Breaker sale</title><link>https://shop/b</link><guid>b</guid></item></channel></rss>"""
ATOM = b"""<feed xmlns="http://www.w3.org/2005/Atom"><entry><id>tag:1</id><title>v1.2</title>
<link rel="alternate" href="https://github.com/o/r/releases/v1.2"/><updated>2026-09-01</updated></entry></feed>"""


def page(*lines: str) -> Page:
    return Page("https://shop.example/panel", "Panel", list(lines))


class PageTests(unittest.TestCase):

    def test_html_becomes_readable_text_without_scripts_or_styles(self) -> None:
        fetched = Fetched("https://shop.example/panel", "text/html; charset=utf-8", HTML)
        with mock.patch.object(tool, "fetch", return_value=fetched):
            text = tool.page_text("https://shop.example/panel", links=True)
        self.assertTrue(text.startswith("Panel shop\nhttps://shop.example/panel"))
        self.assertIn("# 200A panel\nPrice: $450\n- In stock\n- Ships in 2 days", text)
        self.assertNotIn("hidden", text)
        self.assertIn("- Cart: https://shop.example/cart", text)
        self.assertIn("- the specs: https://example.com/specs", text)

    def test_files_are_not_pages(self) -> None:
        pdf = Fetched("https://x/a.pdf", "application/pdf", b"%PDF")
        with mock.patch.object(tool, "fetch", return_value=pdf), self.assertRaisesRegex(ToolError, "not a web page"):
            tool.read_page("https://x/a.pdf")

    def test_this_servers_addresses_and_other_schemes_are_refused(self) -> None:
        for url in ("http://localhost:8080/api", "http://127.0.0.1/", "http://[::1]/", "http://169.254.169.254/"):
            with self.assertRaisesRegex(ToolError, "address of this server"):
                tool.check_url(url)
        for url in ("file:///etc/passwd", "ftp://example.com/", "https:///nohost"):
            with self.assertRaisesRegex(ToolError, "only http and https"):
                tool.check_url(url)

    def test_a_redirect_to_this_server_is_refused(self) -> None:
        handler = tool.CheckedRedirects()
        request = tool.urllib.request.Request("https://example.com/")
        with self.assertRaisesRegex(ToolError, "address of this server"):
            handler.redirect_request(request, None, 302, "Found", Message(), "http://127.0.0.1:8080/api/v1/cases")


class ConditionTests(unittest.TestCase):

    def test_contains_fires_at_once_when_true_and_absent_waits_for_it_to_go(self) -> None:
        with mock.patch.object(tool, "read_page", return_value=page("Price: $450", "In stock")):
            fired = tool.check_contains({"url": "https://shop.example/panel", "text": "in STOCK"}, None)
            gone = tool.check_contains({"url": "https://shop.example/panel", "text": "In stock", "absent": True}, None)
        self.assertEqual("fired", fired["status"])
        self.assertEqual(["In stock"], fired["events"][0]["lines"])  # pyright: ignore[reportIndexIssue]
        self.assertEqual("pending", gone["status"])

    def test_changed_records_first_then_reports_added_and_removed_lines(self) -> None:
        params: Json = {"url": "https://shop.example/panel", "around": "price"}
        with mock.patch.object(tool, "read_page", return_value=page("Price: $450", *[f"filler {n}" for n in range(9)])):
            first = tool.check_changed(params, None)
            same = tool.check_changed(params, first["cursor"])
        far_change = page("Price: $450", *[f"filler {n}" for n in range(8)], "a new ad")
        with mock.patch.object(tool, "read_page", return_value=far_change):
            outside = tool.check_changed(params, first["cursor"])
        with mock.patch.object(tool, "read_page", return_value=page("Price: $399", "filler 0")):
            changed = tool.check_changed(params, first["cursor"])
        self.assertEqual(("pending", "pending", "pending"), (first["status"], same["status"], outside["status"]))
        self.assertEqual("fired", changed["status"])
        event = changed["events"][0]  # pyright: ignore[reportIndexIssue]
        self.assertEqual((["Price: $399"], ["Price: $450"]), (event["added"], event["removed"][:1]))

    def test_feed_fires_only_for_items_new_since_the_wait_started(self) -> None:
        params: Json = {"url": "https://shop.example/feed", "contains": "stock"}
        old = Fetched("https://shop.example/feed", "application/rss+xml", RSS.replace(b"<item><title>Panel", b"<x><title>P").replace(b"</pubDate></item>", b"</pubDate></x>"))
        with mock.patch.object(tool, "fetch", return_value=old):
            first = tool.check_feed(params, None)
        with mock.patch.object(tool, "fetch", return_value=Fetched("u", "application/rss+xml", RSS)):
            fired = tool.check_feed(params, first["cursor"])
            again = tool.check_feed(params, fired["cursor"])
        self.assertEqual("pending", first["status"])
        self.assertEqual("fired", fired["status"])
        items = fired["events"][0]["new_items"]  # pyright: ignore[reportIndexIssue]
        self.assertEqual([("Panel back in stock", "https://shop/a")], [(i["title"], i["link"]) for i in items])
        self.assertEqual("pending", again["status"])

    def test_atom_entries_and_bad_feeds(self) -> None:
        items = tool.feed_items(Fetched("u", "application/atom+xml", ATOM))
        self.assertEqual([{"id": "tag:1", "title": "v1.2", "link": "https://github.com/o/r/releases/v1.2",
                           "date": "2026-09-01"}], items)
        with self.assertRaisesRegex(ToolError, "not an RSS or Atom feed"):
            tool.feed_items(Fetched("u", "text/html", b"<html><p>no</html"))

    def test_missing_params_are_errors(self) -> None:
        with self.assertRaisesRegex(ToolError, "`url` is required"):
            tool.check_changed({}, None)


class Reply:
    """What urlopen returns, for a canned body."""

    def __init__(self, body: bytes) -> None:
        self.body = body

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_: object) -> None:
        pass

    def read(self) -> bytes:
        return self.body


class SearchTests(unittest.TestCase):
    ENV: ClassVar[dict[str, str]] = {"GOOGLE_CSE_API_KEY": "k3y", "GOOGLE_CSE_ID": "cx1"}

    def test_results_and_the_next_page(self) -> None:
        reply = {"searchInformation": {"totalResults": "1200"},
                 "items": [{"title": "Open-Meteo", "link": "https://open-meteo.com/", "snippet": "Free\n weather API"}]}
        with mock.patch("urllib.request.urlopen", return_value=Reply(json.dumps(reply).encode())) as opened:
            found = tool.search(self.ENV, "weather api", 1, start=11)
        url = opened.call_args.args[0].full_url
        self.assertIn("key=k3y", url)
        self.assertIn("cx=cx1", url)
        self.assertIn("start=11", url)
        self.assertIn("safe=off", url)
        self.assertEqual([{"title": "Open-Meteo", "url": "https://open-meteo.com/", "snippet": "Free weather API"}],
                         found["results"])
        self.assertEqual((found["total"], found["next_start"]), ("1200", 12))

    def test_location_and_filters_become_google_parameters_over_configured_defaults(self) -> None:
        env = {**self.ENV, "GOOGLE_CSE_GL": "ca", "GOOGLE_CSE_HL": "fr", "GOOGLE_CSE_LR": ""}

        defaults = tool.SearchOptions.with_defaults(env).google()
        chosen = tool.SearchOptions.with_defaults(
            env, country="US", only_country="us", language="en", since="m6", exclude_site="pinterest.com",
            file_type=".PDF", exact="200A", exclude="used", sort_by_date=True, safe=True).google()

        self.assertEqual({"gl": "ca", "hl": "fr", "safe": "off"}, defaults)
        self.assertEqual({"gl": "us", "cr": "countryUS", "lr": "lang_en", "hl": "fr", "dateRestrict": "m6",
                          "siteSearch": "pinterest.com", "siteSearchFilter": "e", "fileType": "pdf",
                          "exactTerms": "200A", "excludeTerms": "used", "sort": "date", "safe": "active"}, chosen)
        for bad in ({"country": "canada"}, {"since": "7d"}, {"site": "a.com", "exclude_site": "b.com"},
                    {"file_type": "p d f"}):
            with self.assertRaises(ToolError):
                tool.SearchOptions.with_defaults(env, **bad).google()

    def test_errors_explain_and_never_show_the_key(self) -> None:
        with self.assertRaisesRegex(ToolError, "not configured"):
            tool.search({}, "q", 3)
        body = json.dumps({"error": {"code": 403, "message": "API key k3y is not valid"}}).encode()
        error = urllib.error.HTTPError("https://g/?key=k3y", 403, "Forbidden", Message(), io.BytesIO(body))
        with mock.patch("urllib.request.urlopen", side_effect=error), self.assertRaises(ToolError) as caught:
            tool.search(self.ENV, "q", 3)
        self.assertEqual("Google search failed (HTTP 403): API key *** is not valid", str(caught.exception))
        for count in (0, 11):
            with self.assertRaisesRegex(ToolError, "count"):
                tool.search(self.ENV, "q", count)


@unittest.skipUnless("1" == os.environ.get("WEB_LIVE_TEST"), "set WEB_LIVE_TEST=1 to use the real web")
class LiveTests(unittest.TestCase):

    def test_check_config_passes(self) -> None:
        from check_config import HERE, live_checks, load_env
        checks = live_checks(load_env(HERE / "config.toml", Path("/run/secrets")))
        self.assertTrue(all(check.ok or not check.required for check in checks), checks)


if __name__ == "__main__":
    unittest.main()
