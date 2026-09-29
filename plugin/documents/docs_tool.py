#!/usr/bin/env python3
"""clankjob documents plugin: metadata, OCR and text extraction for a case's files.

A command plugin (design section 9.9): each tool call runs one subcommand on a case file
(the server passes its path) and prints JSON or text. Standard library only; it drives
programs installed in the clankjob image:

    metadata FILE              exiftool + file: type, dimensions, dates, camera, GPS, author…
    pdf-info FILE              pdfinfo: pages, title, author, producer, dates, encryption
    ocr FILE [--lang L] [--first N] [--last N]
                               tesseract; scanned PDFs are rendered page by page (pdftoppm)
    to-text FILE               pdftotext for PDFs, pandoc for Word, ODT, RTF, HTML, EPUB…
    media-info FILE            ffprobe: duration, codecs, streams of audio and video
"""

import argparse
import json
import re
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

Json = dict[str, object]

TIMEOUT = 300
MAX_OCR_PAGES = 30
DEFAULT_OCR_PAGES = 10
OCR_LANGUAGE = re.compile(r"^[a-z_]{3,}(\+[a-z_]{3,})*$")
# exiftool fields about the file on this server, not about the document.
NOISE = {"SourceFile", "Directory", "FileName", "FilePermissions", "ExifToolVersion", "FileAccessDate",
         "FileInodeChangeDate", "FileModifyDate"}
PANDOC_FORMATS = {".docx": "docx", ".odt": "odt", ".rtf": "rtf", ".html": "html", ".htm": "html",
                  ".epub": "epub", ".md": "markdown", ".rst": "rst", ".tex": "latex", ".org": "org",
                  ".txt": "markdown", ".csv": "csv", ".ipynb": "ipynb", ".pptx": "pptx", ".xlsx": "xlsx"}


class ToolError(Exception):
    """A failure reported to the LLM (printed to stderr, exit status 1)."""


def need(program: str) -> str:
    path = shutil.which(program)
    if path is None:
        raise ToolError(f"`{program}` is not installed on this server (it is part of the clankjob image)")
    return path


def run(command: list[str], binary: bool = False) -> str:
    try:
        done = subprocess.run(command, capture_output=True, timeout=TIMEOUT, check=False)
    except subprocess.TimeoutExpired as error:
        raise ToolError(f"`{Path(command[0]).name}` took longer than {TIMEOUT}s") from error
    if 0 != done.returncode:
        detail = done.stderr.decode(errors="replace").strip()[-800:]
        raise ToolError(f"`{Path(command[0]).name}` failed: {detail or f'exit status {done.returncode}'}")
    return "" if binary else done.stdout.decode(errors="replace")


def mime_type(path: Path) -> str:
    # --dereference: the server passes case files as symlinks, which `file` would describe as such.
    return run([need("file"), "--brief", "--mime-type", "--dereference", str(path)]).strip()


def metadata(path: Path) -> Json:
    result: Json = {"media_type": mime_type(path), "size": path.stat().st_size}
    if shutil.which("exiftool") is None:
        result["note"] = "exiftool is not installed: only the type is known"
        return result
    parsed = json.loads(run([need("exiftool"), "-json", "-G1", "-a", "-s", str(path)]) or "[]")
    fields = parsed[0] if isinstance(parsed, list) and parsed and isinstance(parsed[0], dict) else {}
    result["metadata"] = {key: value for key, value in fields.items()
                          if key.split(":", 1)[-1] not in NOISE and not key.startswith("ExifTool:")}
    return result


def pdf_info(path: Path) -> Json:
    info: Json = {}
    for line in run([need("pdfinfo"), str(path)]).splitlines():
        if ":" in line:
            key, value = line.split(":", 1)
            info[key.strip()] = value.strip()
    pages = str(info.get("Pages", ""))
    if pages.isdigit():
        info["Pages"] = int(pages)
    return info


def ocr(path: Path, lang: str, first: int, last: int | None) -> str:
    if not OCR_LANGUAGE.match(lang):
        raise ToolError(f"lang must be tesseract language codes like `eng` or `eng+fra`, got {lang!r}")
    tesseract = need("tesseract")
    if "application/pdf" != mime_type(path):
        return run([tesseract, str(path), "stdout", "-l", lang]).strip()
    pages = pdf_info(path).get("Pages")
    total = pages if isinstance(pages, int) else 1
    first = max(1, first)
    last = min(total, last if last is not None else first + DEFAULT_OCR_PAGES - 1, first + MAX_OCR_PAGES - 1)
    if first > total:
        raise ToolError(f"the PDF has {total} pages")
    texts: list[str] = []
    with tempfile.TemporaryDirectory(prefix="clankjob-ocr-") as work:
        run([need("pdftoppm"), "-r", "200", "-png", "-f", str(first), "-l", str(last), str(path),
             str(Path(work) / "page")], binary=True)
        for number, image in enumerate(sorted(Path(work).glob("page*.png")), start=first):
            text = run([tesseract, str(image), "stdout", "-l", lang]).strip()
            texts.append(f"--- page {number} ---\n{text}")
    more = f"\n\n(pages {first}-{last} of {total}; ask for `first`/`last` to read more)" if last < total else ""
    return "\n\n".join(texts) + more


def to_text(path: Path) -> str:
    kind = mime_type(path)
    if "application/pdf" == kind:
        text = run([need("pdftotext"), "-layout", str(path), "-"])
        if not text.strip():
            raise ToolError("this PDF has no text layer (probably a scan): use `ocr` instead")
        return text
    source = PANDOC_FORMATS.get(path.suffix.lower())
    if source is None:
        raise ToolError(f"cannot convert {kind} ({path.suffix or 'no extension'}) to text")
    return run([need("pandoc"), "--from", source, "--to", "plain", "--wrap=none", str(path)])


def media_info(path: Path) -> Json:
    probe = json.loads(run([need("ffprobe"), "-v", "error", "-print_format", "json", "-show_format",
                            "-show_streams", str(path)]))
    return probe if isinstance(probe, dict) else {}


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    for name in ("metadata", "pdf-info", "to-text", "media-info"):
        sub.add_parser(name).add_argument("file", type=Path)
    ocr_parser = sub.add_parser("ocr")
    ocr_parser.add_argument("file", type=Path)
    ocr_parser.add_argument("--lang", default="eng+fra")
    ocr_parser.add_argument("--first", type=int, default=1)
    ocr_parser.add_argument("--last", type=int)
    return parser


def main() -> int:
    args = build_parser().parse_args()
    try:
        path: Path = args.file
        if not path.is_file():
            raise ToolError(f"{path.name} cannot be read")
        if "metadata" == args.command:
            print(json.dumps(metadata(path), ensure_ascii=False, default=str))
        elif "pdf-info" == args.command:
            print(json.dumps(pdf_info(path), ensure_ascii=False))
        elif "media-info" == args.command:
            print(json.dumps(media_info(path), ensure_ascii=False))
        elif "ocr" == args.command:
            print(ocr(path, args.lang, args.first, args.last))
        else:
            print(to_text(path))
    except ToolError as error:
        print(f"Error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except KeyboardInterrupt:
        sys.exit(130)
