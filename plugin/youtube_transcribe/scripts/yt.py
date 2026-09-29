#!/usr/bin/env -S uv run --script
# /// script
# requires-python = ">=3.11"
# dependencies = [
#   "youtube-transcript-api>=1.0",
#   "httpx>=0.27",
#   "tabulate>=0.9",
# ]
# ///
"""
yt.py — YouTube transcript fetcher.

Downloads caption/transcript text for a YouTube video via the same timedtext
endpoint the web player uses (youtube-transcript-api). No API key required.

Note: this is an unofficial endpoint. The official Data API v3
`captions.download` only works for videos you own, so it is not usable here.

Usage:
    ./yt.py <command> <video> [options]

`<video>` may be a full URL (watch, youtu.be, shorts, embed, live) or a bare
11-character video ID.

Commands:
  transcript  Download the transcript (default command)
  langs       List the caption tracks available for a video
  info        Video title / channel / URL (public oEmbed endpoint)

Examples:
    ./yt.py transcript "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
    ./yt.py transcript dQw4w9WgXcQ --format stamped --chunk 120
    ./yt.py transcript dQw4w9WgXcQ --out /tmp/call.txt
    ./yt.py transcript dQw4w9WgXcQ --lang de --translate en
    ./yt.py langs dQw4w9WgXcQ

If YouTube blocks the request (common from datacenter/VPN IPs), pass a
residential proxy with --proxy http://user:pass@host:port, or export
YT_PROXY_URL.
"""

import argparse
import json
import os
import re
import sys
import textwrap

import httpx
from tabulate import tabulate

OEMBED_URL = "https://www.youtube.com/oembed"

# watch?v=ID, youtu.be/ID, /shorts/ID, /embed/ID, /live/ID, /v/ID
ID_PATTERNS = [
    re.compile(r"[?&]v=([0-9A-Za-z_-]{11})"),
    re.compile(r"youtu\.be/([0-9A-Za-z_-]{11})"),
    re.compile(r"/(?:shorts|embed|live|v)/([0-9A-Za-z_-]{11})"),
]
BARE_ID = re.compile(r"^[0-9A-Za-z_-]{11}$")


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def video_id(value: str) -> str:
    """Extract an 11-char video ID from a URL, or pass through a bare ID."""
    value = value.strip()
    if BARE_ID.match(value):
        return value
    for pattern in ID_PATTERNS:
        match = pattern.search(value)
        if match:
            return match.group(1)
    raise ValueError(f"could not extract a YouTube video ID from {value!r}")


def timestamp(seconds: float, *, millis: bool = False, sep: str = ",") -> str:
    total = int(seconds)
    hh, mm, ss = total // 3600, (total % 3600) // 60, total % 60
    if millis:
        ms = int(round((seconds - total) * 1000))
        return f"{hh:02d}:{mm:02d}:{ss:02d}{sep}{ms:03d}"
    if hh:
        return f"{hh:d}:{mm:02d}:{ss:02d}"
    return f"{mm:02d}:{ss:02d}"


def build_api(args):
    """Instantiate YouTubeTranscriptApi with optional proxy / cookie config."""
    from youtube_transcript_api import YouTubeTranscriptApi

    kwargs = {}
    proxy = args.proxy or os.environ.get("YT_PROXY_URL")
    if proxy:
        from youtube_transcript_api.proxies import GenericProxyConfig

        kwargs["proxy_config"] = GenericProxyConfig(http_url=proxy, https_url=proxy)
    if getattr(args, "cookies", None):
        kwargs["cookie_path"] = args.cookies
    return YouTubeTranscriptApi(**kwargs)


def fetch_meta(vid: str) -> dict:
    """Best-effort title/channel lookup. Never fatal — captions are the point."""
    try:
        resp = httpx.get(
            OEMBED_URL,
            params={"url": f"https://www.youtube.com/watch?v={vid}", "format": "json"},
            timeout=10,
            follow_redirects=True,
        )
        resp.raise_for_status()
        return resp.json()
    except Exception:
        return {}


def explain(exc: Exception) -> str:
    """Turn library exceptions into something actionable."""
    name = type(exc).__name__
    hints = {
        "TranscriptsDisabled": "the uploader disabled captions on this video",
        "NoTranscriptFound": "no caption track matched the requested language "
                             "(run `langs` to see what exists)",
        "VideoUnavailable": "the video is private, deleted, or region-blocked",
        "AgeRestricted": "the video is age-restricted; pass --cookies with an "
                         "exported cookies.txt from a logged-in browser session",
        "RequestBlocked": "YouTube blocked this IP (common on cloud/VPN IPs); "
                          "retry from a residential IP or pass --proxy",
        "IpBlocked": "YouTube blocked this IP (common on cloud/VPN IPs); "
                     "retry from a residential IP or pass --proxy",
    }
    hint = hints.get(name)
    return f"{name}: {hint}" if hint else f"{name}: {exc}"


# ---------------------------------------------------------------------------
# Formatting
# ---------------------------------------------------------------------------

def paragraphs(snippets, chunk: float):
    """Group snippets into ~`chunk`-second paragraphs. Yields (start, text)."""
    if chunk <= 0:
        yield snippets[0].start, " ".join(" ".join(s.text.split()) for s in snippets)
        return
    start = snippets[0].start
    buffer: list[str] = []
    for snip in snippets:
        if buffer and snip.start - start >= chunk:
            yield start, " ".join(buffer)
            start, buffer = snip.start, []
        text = " ".join(snip.text.split())  # captions wrap mid-sentence
        if text:
            buffer.append(text)
    if buffer:
        yield start, " ".join(buffer)


def wrap(text: str, width: int) -> str:
    return textwrap.fill(text, width=width) if width > 0 else text


def fmt_text(snippets, args) -> str:
    return "\n\n".join(
        wrap(body, args.width) for _, body in paragraphs(snippets, args.chunk)
    )


def fmt_stamped(snippets, args) -> str:
    blocks = []
    for start, body in paragraphs(snippets, args.chunk):
        blocks.append(f"[{timestamp(start)}] {wrap(body, args.width)}")
    return "\n\n".join(blocks)


def fmt_srt(snippets, args) -> str:
    lines = []
    for i, snip in enumerate(snippets, start=1):
        end = snip.start + snip.duration
        if i < len(snippets):  # clamp so cues never overlap
            end = min(end, snippets[i].start)
        lines.append(
            f"{i}\n"
            f"{timestamp(snip.start, millis=True)} --> {timestamp(end, millis=True)}\n"
            f"{snip.text.strip()}\n"
        )
    return "\n".join(lines)


def fmt_vtt(snippets, args) -> str:
    body = fmt_srt(snippets, args).replace(",", ".")
    return "WEBVTT\n\n" + body


def fmt_json(snippets, args) -> str:
    return json.dumps(
        [
            {"start": s.start, "duration": s.duration, "text": s.text}
            for s in snippets
        ],
        indent=2,
        ensure_ascii=False,
    )


FORMATTERS = {
    "text": fmt_text,
    "stamped": fmt_stamped,
    "srt": fmt_srt,
    "vtt": fmt_vtt,
    "json": fmt_json,
}


# ---------------------------------------------------------------------------
# Commands
# ---------------------------------------------------------------------------

def cmd_transcript(args) -> None:
    vid = video_id(args.video)
    api = build_api(args)
    languages = args.lang or ["en"]

    if args.translate:
        track = api.list(vid).find_transcript(languages)
        if track.language_code != args.translate:
            if not track.is_translatable:
                raise RuntimeError(
                    f"track '{track.language_code}' is not translatable"
                )
            track = track.translate(args.translate)
        fetched = track.fetch()
    else:
        fetched = api.fetch(vid, languages=languages)

    snippets = list(fetched)
    if not snippets:
        raise RuntimeError("transcript is empty")

    meta = {} if args.no_meta else fetch_meta(vid)
    body = FORMATTERS[args.format](snippets, args)

    parts = []
    if args.format in ("text", "stamped") and not args.no_meta:
        header = [
            f"# {meta.get('title', vid)}",
            "",
            f"- Channel: {meta.get('author_name', 'unknown')}",
            f"- URL: https://www.youtube.com/watch?v={vid}",
            f"- Track: {fetched.language} ({fetched.language_code})"
            f"{' — auto-generated' if fetched.is_generated else ''}",
            f"- Length: {timestamp(snippets[-1].start + snippets[-1].duration)}"
            f" · {sum(len(s.text.split()) for s in snippets):,} words",
            "",
            "---",
            "",
        ]
        parts.append("\n".join(header))
    parts.append(body)
    out = "\n".join(parts).rstrip() + "\n"

    if args.out:
        with open(args.out, "w", encoding="utf-8") as fh:
            fh.write(out)
        print(f"Wrote {len(out):,} chars to {args.out}", file=sys.stderr)
    else:
        sys.stdout.write(out)


def cmd_langs(args) -> None:
    vid = video_id(args.video)
    tracks = build_api(args).list(vid)
    rows = [
        [
            t.language_code,
            t.language,
            "auto" if t.is_generated else "manual",
            "yes" if t.is_translatable else "no",
        ]
        for t in tracks
    ]
    if not rows:
        print("No caption tracks found.")
        return
    print(f"Caption tracks for {vid}\n")
    print(tabulate(rows, headers=["Code", "Language", "Source", "Translatable"]))


def cmd_info(args) -> None:
    vid = video_id(args.video)
    meta = fetch_meta(vid)
    if not meta:
        raise RuntimeError("oEmbed lookup failed (video private or unavailable?)")
    rows = [
        ["Video ID", vid],
        ["Title", meta.get("title", "")],
        ["Channel", meta.get("author_name", "")],
        ["URL", f"https://www.youtube.com/watch?v={vid}"],
    ]
    print(tabulate(rows, tablefmt="plain"))


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        description="Download YouTube transcripts.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    sub = parser.add_subparsers(dest="command")

    def common(p):
        p.add_argument("video", help="YouTube URL or 11-char video ID")
        p.add_argument("--proxy", help="proxy URL (or set YT_PROXY_URL)")
        p.add_argument("--cookies", help="path to a cookies.txt file")
        return p

    t = common(sub.add_parser("transcript", help="download the transcript"))
    t.add_argument("--lang", action="append", metavar="CODE",
                   help="preferred language code, repeatable in priority order "
                        "(default: en)")
    t.add_argument("--translate", metavar="CODE",
                   help="translate the track to this language code")
    t.add_argument("--format", choices=list(FORMATTERS), default="text",
                   help="output format (default: text)")
    t.add_argument("--chunk", type=float, default=60,
                   help="paragraph length in seconds for text/stamped formats; "
                        "0 = one paragraph (default: 60)")
    t.add_argument("--width", type=int, default=100,
                   help="wrap column for text/stamped, 0 to disable (default: 100)")
    t.add_argument("--out", metavar="FILE", help="write to FILE instead of stdout")
    t.add_argument("--no-meta", action="store_true",
                   help="skip the title/channel header lookup")

    common(sub.add_parser("langs", help="list available caption tracks"))
    common(sub.add_parser("info", help="video title and channel"))
    return parser


def main() -> None:
    parser = build_parser()
    # bare `./yt.py <video>` behaves as `./yt.py transcript <video>`
    argv = sys.argv[1:]
    if argv and argv[0] not in {"transcript", "langs", "info", "-h", "--help"}:
        argv.insert(0, "transcript")
    args = parser.parse_args(argv)
    if not args.command:
        parser.print_help()
        sys.exit(1)

    dispatch = {
        "transcript": cmd_transcript,
        "langs": cmd_langs,
        "info": cmd_info,
    }
    try:
        dispatch[args.command](args)
    except Exception as exc:
        print(f"Error: {explain(exc)}", file=sys.stderr)
        sys.exit(1)


if __name__ == "__main__":
    main()
