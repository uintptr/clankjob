#!/usr/bin/env python3
"""Tests for the documents plugin, with fake programs on PATH. Run: python3 -m unittest -v test_docs_tool.py"""

import json
import os
import tempfile
import unittest
from pathlib import Path
from typing import Self

import docs_tool as tool
from docs_tool import ToolError


class FakePrograms:
    """A directory of shell scripts standing in for exiftool, pdfinfo, file…, put first on PATH."""

    def __init__(self) -> None:
        self.dir = tempfile.TemporaryDirectory()
        self.path = Path(self.dir.name)
        self.saved = os.environ["PATH"]
        os.environ["PATH"] = f"{self.path}{os.pathsep}{self.saved}"

    def add(self, name: str, body: str) -> None:
        script = self.path / name
        script.write_text(f"#!/bin/sh\n{body}\n")
        script.chmod(0o755)

    @property
    def document(self) -> Path:
        """A stand-in case file, as the server would link it."""
        path = self.path / "quote.pdf"
        if not path.exists():
            path.write_text("%PDF-1.7 fake")
        return path

    def __enter__(self) -> Self:
        return self

    def __exit__(self, *_: object) -> None:
        os.environ["PATH"] = self.saved
        self.dir.cleanup()


class DocsToolTests(unittest.TestCase):

    def test_metadata_keeps_the_document_facts_and_drops_server_noise(self) -> None:
        exiftool_output = json.dumps([{
            "SourceFile": "/tmp/x", "System:FileName": "x", "System:FilePermissions": "-rw",
            "ExifTool:ExifToolVersion": 13, "EXIF:Model": "Pixel 8", "Composite:GPSPosition": "45.5 N, 73.6 W",
        }])
        with FakePrograms() as programs:
            programs.add("file", "echo image/jpeg")
            programs.add("exiftool", f"echo '{exiftool_output}'")

            result = tool.metadata(programs.document)

            self.assertEqual(result["media_type"], "image/jpeg")
            self.assertEqual(result["metadata"], {"EXIF:Model": "Pixel 8", "Composite:GPSPosition": "45.5 N, 73.6 W"})

    def test_the_type_is_the_linked_files_not_the_links(self) -> None:
        with FakePrograms() as programs:
            programs.add("file", 'case "$*" in *--dereference*) echo application/pdf ;; *) echo inode/symlink ;; esac')

            self.assertEqual(tool.mime_type(programs.document), "application/pdf")

    def test_pdf_info_is_parsed_into_fields(self) -> None:
        with FakePrograms() as programs:
            programs.add("pdfinfo", "printf 'Title:          Quote 42\\nPages:          3\\nEncrypted:      no\\n'")

            info = tool.pdf_info(programs.document)

            self.assertEqual(info, {"Title": "Quote 42", "Pages": 3, "Encrypted": "no"})

    def test_a_pdf_without_text_points_to_ocr(self) -> None:
        with FakePrograms() as programs:
            programs.add("file", "echo application/pdf")
            programs.add("pdftotext", "printf '   \\n'")

            with self.assertRaises(ToolError) as caught:
                tool.to_text(programs.document)

            self.assertIn("use `ocr`", str(caught.exception))

    def test_ocr_languages_are_validated_and_missing_programs_named(self) -> None:
        with FakePrograms() as programs:
            with self.assertRaises(ToolError) as bad_language:
                tool.ocr(programs.document, "eng; rm -rf /", 1, None)
            programs.add("file", "echo image/png")
            os.environ["PATH"] = str(programs.path)
            with self.assertRaises(ToolError) as missing:
                tool.ocr(programs.document, "eng", 1, None)

            self.assertIn("language codes", str(bad_language.exception))
            self.assertIn("`tesseract` is not installed", str(missing.exception))

    def test_failures_carry_the_programs_error(self) -> None:
        with FakePrograms() as programs:
            programs.add("pdfinfo", "echo 'Syntax Error: Could not find trailer dictionary' >&2; exit 1")

            with self.assertRaises(ToolError) as caught:
                tool.pdf_info(programs.document)

            self.assertIn("trailer dictionary", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
