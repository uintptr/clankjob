# Documents plugin (metadata, OCR, text)

Gives cases tools that work on their own files: what a photo's metadata says (camera,
date taken, GPS position), what's in a scanned quote (OCR), what a Word document says.
The server hands each tool the case file the agent names; nothing leaves the machine.

| Tool               | What it does                                                                    | Uses                    |
| ------------------ | ------------------------------------------------------------------------------- | ----------------------- |
| `file_metadata`    | Type, size and every metadata field: EXIF, GPS, XMP, PDF and office properties  | `exiftool`, `file`      |
| `pdf_info`         | Pages, title, author, producer, dates, page size, encryption                    | `pdfinfo`               |
| `ocr`              | Text of an image or a scanned PDF (10 pages per call by default, up to 30)      | `tesseract`, `pdftoppm` |
| `document_to_text` | Plain text of Word, ODT, RTF, HTML, EPUB, PowerPoint, Excel, and PDFs with text | `pandoc`, `pdftotext`   |
| `media_info`       | Duration, format, codecs and streams of audio and video                         | `ffprobe`               |

Long results (`ocr`, `document_to_text`) are saved as a case file the agent reads with
`read_file`, like any other long tool output.

## Setup

Nothing to configure. The programs are part of the clankjob Docker image. On another
machine, install what the check reports missing:

```sh
./check_config.py
```

It lists each program (with its Debian package when missing), the Tesseract languages
(English and French), and reads back a line of text drawn into an image to prove OCR
works. A missing program only disables the tools that use it.

Tests use fake programs on `PATH`, so they need none of them:

```sh
python3 -m unittest -v test_docs_tool.py
```

## Design

`docs_tool.py` is a command plugin ([design §9.9](../../docs/design.md)), standard library
only, one subcommand per tool. Each tool takes a `file` argument: the agent names one of
the case's files, and the server passes a path to it, linked under its own name in a
private temporary directory for the duration of the call (the store keeps files by id,
and tools such as `pandoc` go by the extension). Only the case's own files can be named.

- `ocr` detects PDFs by content, renders the requested pages at 200 dpi with `pdftoppm`
  and runs Tesseract on each (`eng+fra` by default; `lang` takes Tesseract codes). The
  result is marked page by page and says which pages remain.
- `document_to_text` uses `pdftotext -layout` for PDFs (keeping tables readable) and
  says to use `ocr` when a PDF has no text layer; other formats go through `pandoc`,
  chosen by extension.
- `file_metadata` drops the fields about the copy on this server (path, permissions,
  access dates) and keeps everything about the document itself.
- Every tool has a timeout (1 to 5 minutes) and reports a program's own error message.

Planned: rendering a PDF page to an image for `view_image`, and transcribing audio.
