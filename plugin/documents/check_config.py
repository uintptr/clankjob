#!/usr/bin/env python3
"""Check the documents plugin on this machine: the programs its tools drive.

The clankjob Docker image has them all; on another machine, install what the checklist
reports missing (Debian package in brackets). When ImageMagick and Tesseract are both
present, it also reads back a line of text drawn into an image, to prove OCR works.
Nothing leaves this machine and nothing is written outside a temporary directory.

    ./check_config.py
"""

import argparse
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

# program, what it is for, Debian package, whether the plugin is useless without it
PROGRAMS = (
    ("file", "detecting file types", "file", True),
    ("exiftool", "file_metadata", "libimage-exiftool-perl", False),
    ("pdfinfo", "pdf_info, ocr of PDFs", "poppler-utils", False),
    ("pdftotext", "document_to_text of PDFs", "poppler-utils", False),
    ("pdftoppm", "ocr of scanned PDFs", "poppler-utils", False),
    ("tesseract", "ocr", "tesseract-ocr", False),
    ("pandoc", "document_to_text of Word, ODT, RTF, HTML…", "pandoc", False),
    ("ffprobe", "media_info", "ffmpeg", False),
)
LANGUAGES = (("eng", "tesseract-ocr-eng"), ("fra", "tesseract-ocr-fra"))
SAMPLE = "clankjob 1450"


@dataclass(frozen=True)
class Check:
    ok: bool
    required: bool
    what: str
    detail: str = ""


def output(command: list[str]) -> tuple[bool, str]:
    try:
        done = subprocess.run(command, capture_output=True, text=True, timeout=120, check=False)
    except (OSError, subprocess.TimeoutExpired) as error:
        return False, str(error)
    return 0 == done.returncode, done.stdout + done.stderr


def a_font() -> Path | None:
    """A TrueType font to draw the test text with: ImageMagick aborts on its default
    font when no Ghostscript fonts are installed, as in the slim image."""
    fonts = sorted(Path("/usr/share/fonts").rglob("*.ttf")) if Path("/usr/share/fonts").is_dir() else []
    preferred = [font for font in fonts if "DejaVuSans." in font.name]
    return (preferred or fonts or [None])[0]


def checks() -> list[Check]:
    found: list[Check] = []
    for program, used_for, package, required in PROGRAMS:
        path = shutil.which(program)
        found.append(Check(path is not None, required, f"{program} ({used_for})",
                           "" if path else f"install it: {package}"))
    if shutil.which("tesseract"):
        ok, text = output(["tesseract", "--list-langs"])
        installed = set(text.split()) if ok else set()
        for language, package in LANGUAGES:
            found.append(Check(language in installed, False, f"tesseract language {language}",
                               "" if language in installed else f"install it: {package}"))
    magick = shutil.which("magick") or shutil.which("convert")
    if magick and shutil.which("tesseract"):
        with tempfile.TemporaryDirectory(prefix="clankjob-check-") as work:
            image = Path(work) / "sample.png"
            font = a_font()
            drawn, detail = output([magick, "-size", "600x120", "xc:white", "-fill", "black",
                                    *(["-font", str(font)] if font else []), "-pointsize", "48",
                                    "-annotate", "+20+80", SAMPLE, str(image)])
            drawn = drawn and image.is_file()
            if drawn:
                ok, text = output(["tesseract", str(image), "stdout", "-l", "eng"])
                read_back = ok and SAMPLE in " ".join(text.split())
                found.append(Check(read_back, False, f"OCR reads back \"{SAMPLE}\"",
                                   "" if read_back else (text.strip()[-300:] or "tesseract read nothing")))
            else:
                found.append(Check(False, False, "OCR test image",
                                   detail.strip()[-300:] or "ImageMagick could not draw it (no usable font?)"))
    return found


def main() -> int:
    argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter).parse_args()
    results = checks()
    for check in results:
        mark = "ok  " if check.ok else ("FAIL" if check.required else "warn")
        print(f"  {mark} {check.what}")
        if check.detail:
            print(f"         fix: {check.detail}")
    return 0 if all(check.ok or not check.required for check in results) else 1


if __name__ == "__main__":
    sys.exit(main())
