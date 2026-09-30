# Web plugin (search, pages, watches)

Lets the agent search the web, read pages, and wait for something to happen on the web:
a product back in stock, a page that changes, a new item in a feed. Waiting costs nothing
while it waits: the server checks the page about once an hour and wakes the case only
when the condition is met.

| Tool / condition    | What it does                                                                         |
| ------------------- | ------------------------------------------------------------------------------------ |
| `web_search`        | Google search (Custom Search JSON API): titles, URLs, snippets; localized by country |
| `web_page`          | A page as plain text (title, headings, paragraphs, lists), optionally its links      |
| `web_page_contains` | Wait until a page contains a text (or, with `absent`, until it no longer does)       |
| `web_page_changed`  | Wait until a page's text changes, optionally only near a text (a price, a status)    |
| `web_feed_item`     | Wait for a new item in an RSS or Atom feed, optionally with a text in its title      |

`web_page` and the conditions need no setup. `web_search` needs a Google API key.

## Setup

1. Create a [Programmable Search Engine](https://programmablesearchengine.google.com) set
   to search the entire web. Its **Search engine ID** is the `cx`.
2. In the [Google Cloud console](https://console.cloud.google.com), enable the
   **Custom Search API** and create an API key (restrict it to that API). The first 100
   queries a day are free.
3. Copy `config.example.toml` to `config.toml` and set `GOOGLE_CSE_ID`, the default
   country (`GOOGLE_CSE_GL`) and, if you like, languages. The key goes in `.env` as
   `GOOGLE_CSE_API_KEY` (the setup's `--configure web` asks for all of it).

Google has announced that the Custom Search JSON API is closed to new customers and will
be retired for existing ones; the check below tells whether your key still works.

```sh
export GOOGLE_CSE_API_KEY=...    # when config.toml uses { env = "GOOGLE_CSE_API_KEY" }
./check_config.py
```

```
  ok   web_page reads https://example.com/ ('Example Domain', 2 lines)
  ok   web_feed_item reads a feed (10 items in https://github.com/python/cpython/releases.atom)
  ok   this server's own addresses are refused
  ok   web_search through Google (engine 0123…): first result https://open-meteo.com/
```

The search counts toward the daily quota. Tests fake the web and Google;
`WEB_LIVE_TEST=1` also runs the checks above:

```sh
python3 -m unittest -v test_web_tool.py
```

## Design

`web_tool.py` is a command plugin ([design §9.9](../../docs/design.md)), standard library
only, one subcommand per tool and condition.

- **Search.** `web_search` calls `customsearch/v1` with the key and `cx`, at most 10
  results per call (`start` pages through the first 100). Location is by country: `gl`
  (`country`) favors a country and `cr` (`only_country`) restricts to one. Google has no
  finer location, so the tool description tells the agent to put the city in the query.
  Defaults for `gl`, `hl` and `lr` come from `GOOGLE_CSE_GL`, `_HL` and `_LR`; the agent
  can override them, and narrow by date (`since`), site, file type, exact or excluded
  terms, sort by date, and turn on SafeSearch. The result echoes the parameters used
  (`options`). The key is replaced by `***` in any error.
- **Pages.** Fetched with a browser-like user agent, at most 5 MB, 30 s. HTML becomes
  text with the standard library's parser: scripts, styles, `<head>` and SVG are dropped,
  headings become `#` lines, list items `- ` lines. Plain text and JSON pass through;
  other types (PDF, images) are refused with a pointer to `run_command`. Long pages are
  stored as a case file (`output = "auto"`).
- **Safety.** The plugin runs in the server's container, so a URL that resolves to this
  machine (loopback, link-local such as a cloud metadata address, unspecified) is
  refused, and so is every redirect to one. The local network is allowed, as from the
  sandbox. Only `http` and `https`.
- **Conditions** use the command-plugin protocol (§9.9): `{"params", "cursor"}` on stdin.
  `web_page_changed` and `web_feed_item` record the page or the feed's item ids on the
  first check, then compare; `web_page_contains` fires on any check where it holds,
  including the first. A change is reported as the lines added and removed (20 each at
  most). They are checked right away, then at most every 10 minutes (`min_interval`) and
  hourly by default (`interval`), so a watched site sees few requests. A page that cannot
  be fetched counts as a failed check; after 5 in a row the case wakes with the error.

Planned: pages that need JavaScript (a headless browser in the sandbox), a price
condition that reads a number near a text, and watching several pages at once.
