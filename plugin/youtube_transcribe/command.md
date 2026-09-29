Download a YouTube transcript and analyse it through the Bezos & Buffett frameworks in this repository (e.g. `/transcript https://www.youtube.com/watch?v=XXXXXXXXXXX` or `/transcript XXXXXXXXXXX NVDA earnings call`).

**Arguments:**
- `$VIDEO` (required) — a YouTube URL (watch, youtu.be, shorts, embed, live) or a bare 11-char video ID
- Everything after the URL is free-form context: a ticker, a period label, or a focus for the analysis (e.g. `NVDA Q2 FY2027`, `focus on capex commentary`). Optional.

---

## Step 1 — Fetch the transcript

```bash
scripts/yt.py info $VIDEO
scripts/yt.py langs $VIDEO
```

If `langs` shows an English track, pull it. Save the raw transcript to `transcripts/` so the analysis is reproducible and the download is not repeated on follow-up questions:

```bash
mkdir -p transcripts
scripts/yt.py transcript $VIDEO --format stamped --chunk 120 --out transcripts/<slug>.md
```

Name `<slug>` `<TICKER>_<PERIOD>_call` for an earnings call (e.g. `NVDA_Q2_FY2027_call`), otherwise `<channel>_<short-title>_<YYYY-MM-DD>`, lowercased and kebab-cased. Then `Read` the saved file.

**Notes:**
- Use `--format stamped` (default `--chunk 120`) so every claim can be cited with a `[MM:SS]` timestamp. Drop to `--chunk 60` for a dense earnings call, raise to `300` for a long-form interview.
- If no English track exists but a translatable one does: `scripts/yt.py transcript $VIDEO --lang <code> --translate en`.
- If the transcript is long (> ~2 hours), read it in chunks rather than truncating — do not analyse only the opening.

**If the fetch fails**, report the error verbatim and stop; do not substitute recalled knowledge about the video for its actual contents:
- `TranscriptsDisabled` / `NoTranscriptFound` — no captions exist; suggest the IR page or the 8-K exhibit instead
- `RequestBlocked` / `IpBlocked` — YouTube blocked the IP; suggest retrying off-VPN or `--proxy`
- `AgeRestricted` — needs `--cookies` with an exported cookies.txt

## Step 2 — Ground the transcript

Read the framework references before writing the synthesis:
- `jeff_bezos.md`
- `warren_buffet.md`

If a ticker is identifiable (given as an argument, or unambiguous from the video), pull enough hard data to check the speaker's claims against the filings:

```bash
scripts/yf.py $TICKER info
scripts/yf.py $TICKER income --freq quarterly
scripts/yf.py $TICKER cashflow --freq quarterly
scripts/yf.py $TICKER earnings-history
```

Check `analysis/` for an existing file on the same company — if one exists, note where the transcript confirms or contradicts it.

---

## Step 3 — Output

Write the analysis to `analysis/<TICKER>_<PERIOD>_CALL.md` when it is company-specific; otherwise print it inline and offer to save.

### {Video title} — Transcript Analysis
*Channel: {channel} · Video: {URL} · Track: {language, manual or auto-generated} · Length: {duration} · Retrieved: {YYYY-MM-DD}*

> **Caption caveat:** auto-generated tracks mis-transcribe numbers, tickers, and proper nouns. Flag any figure that carries the analysis and verify it against the filing before relying on it.

### 1. What This Is
Two or three sentences: who is speaking, in what capacity, to whom, and why it matters for the investment question.

### 2. TL;DR
Five bullets, each with a `[MM:SS]` citation.

### 3. Claims & Numbers
| Claim | Timestamp | Type | Verifiable against |
|-------|-----------|------|--------------------|
| | `[MM:SS]` | fact / guidance / opinion | 10-Q line item, prior guidance, peer data, unverifiable |

Separate what was *stated as fact* from what was *projected* from what was *spin*. Mark anything the transcript alone cannot settle.

### 4. Management Language
Buffett's test — does this read like an owner writing to partners, or a promoter selling a story? Cite the transcript.
- Owner-orientation: admits mistakes by name, quantifies misses, discusses per-share value, uses plain language
- Promoter tells: adjusted-metric emphasis, passive voice on bad news, "record" framing that hides per-share dilution, unanswered analyst questions, non-GAAP goalpost shifts
- Note evasions and topic changes explicitly, with timestamps

### 5. Bezos & Buffett Read
Structure as **Business Quality → Management → Capital Allocation → Valuation → Risks**. Every framework reference must cite the year (e.g. "Buffett 2007 — Great/Good/Gruesome", "Bezos 2016 — Day 2").

Apply the lenses that the transcript actually supports — do not force all of them:
- *Buffett:* moat direction (widening or narrowing?), incremental return on capital, FCF vs reported earnings, capital allocation decisions disclosed, circle of competence
- *Bezos:* Day 1 vs Day 2 signals, flywheel mechanics named or implied, working-backwards vs skills-forward, experimentation rate, Type 1/Type 2 decision framing, platform potential

### 6. Contradictions & Red Flags
Where the spoken word conflicts with the filings, prior guidance, an earlier file in `analysis/`, or itself. This section is the point of reading transcripts — do not leave it empty out of politeness; if there genuinely are none, say so and say what you checked.

### 7. What To Verify Next
Concrete follow-ups: specific filing line items, the next quarter's disclosure, a peer datapoint, a footnote.

### 8. Scorecard
| Dimension | Rating | Note |
|-----------|--------|------|
| Candour | ★★★☆☆ | |
| Evidence density | ★★★☆☆ | |
| Long-term orientation | ★★★☆☆ | |
| Signal vs promotion | ★★★☆☆ | |

Close with a one-line Buffett verdict and a one-line Bezos verdict.

---
*Transcript retrieved via YouTube's public caption endpoint (`scripts/yt.py`, youtube-transcript-api). Quotes are excerpts for analysis and commentary. This analysis is for informational purposes only and does not constitute investment advice. Verify all figures against primary filings on SEC EDGAR.*

---

## Rules

- **Quote sparingly.** Short excerpts to support a point, never bulk reproduction of the transcript in the analysis file — the raw text already lives in `transcripts/`.
- **Every substantive claim gets a `[MM:SS]` citation.** If you cannot cite it, do not assert it.
- **Never fill gaps from memory.** If the captions garble a number, say so; do not infer what was "probably" said.
- **Distinguish the speaker from the company.** An analyst, a journalist, and a CFO carry different evidentiary weight — say which you are reading.
