#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

"""Unit tests for scripts/check-doc-mermaid.py."""

from __future__ import annotations

import importlib.util
import sys
import tempfile
import unittest
from pathlib import Path
from types import ModuleType


def _load_checker_module() -> ModuleType:
    script_path = Path(__file__).resolve().parent.parent / "check-doc-mermaid.py"
    spec = importlib.util.spec_from_file_location("check_doc_mermaid", script_path)
    if spec is None or spec.loader is None:
        raise RuntimeError(f"Unable to load module spec from {script_path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


checker = _load_checker_module()


class CheckDocMermaidTests(unittest.TestCase):
    """Test Mermaid extraction, syntax checks, accessibility rules, and CLI exit codes."""

    def _write_and_validate(self, content: str) -> list[str]:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            md = root / "docs" / "sample.md"
            md.parent.mkdir(parents=True, exist_ok=True)
            md.write_text(content, encoding="utf-8")
            blocks = checker.extract_mermaid_blocks(md)
            errors: list[str] = []
            for block in blocks:
                errors.extend(checker.validate_block(block, root))
            return errors

    def test_valid_accessible_flowchart_passes(self) -> None:
        md = (
            "# Diagram\n\n"
            "```mermaid\n"
            "flowchart LR\n"
            "  subgraph GROUP[\"Main Flow\"]\n"
            "    A[\"Line 1<br/>Line 2\"] --> B([\"Done\"])\n"
            "  end\n"
            "  classDef ok stroke:#0072B2,stroke-width:2px,fill:none\n"
            "  class B ok\n"
            "```\n"
        )
        self.assertEqual(self._write_and_validate(md), [])

    def test_valid_sequence_and_state_diagrams_pass(self) -> None:
        md = (
            "```mermaid\n"
            "sequenceDiagram\n"
            "  Client->>Server: Request\n"
            "```\n\n"
            "```mermaid\n"
            "stateDiagram-v2\n"
            "  [*] --> Active\n"
            "```\n"
        )
        self.assertEqual(self._write_and_validate(md), [])

    def test_unclosed_fence_fails(self) -> None:
        md = "```mermaid\nflowchart LR\n  A --> B\n"
        errors = self._write_and_validate(md)
        self.assertEqual(len(errors), 1)
        self.assertIn("unclosed ```mermaid fence", errors[0])

    def test_empty_and_comment_only_blocks_fail(self) -> None:
        md = "```mermaid\n%% only a comment\n```\n"
        errors = self._write_and_validate(md)
        self.assertEqual(len(errors), 1)
        self.assertIn("empty mermaid block", errors[0])

    def test_unsupported_diagram_type_fails(self) -> None:
        md = "```mermaid\ngantt\n  title Roadmap\n```\n"
        errors = self._write_and_validate(md)
        self.assertEqual(len(errors), 1)
        self.assertIn("unsupported mermaid diagram type 'gantt'", errors[0])

    def test_hardcoded_fill_override_fails(self) -> None:
        md = (
            "```mermaid\n"
            "flowchart LR\n"
            "  A --> B\n"
            "  classDef bad fill:#ffffff,stroke:#0072B2\n"
            "```\n"
        )
        errors = self._write_and_validate(md)
        self.assertEqual(len(errors), 1)
        self.assertIn("hardcoded fill override", errors[0])

    def test_styling_html_tags_fail_while_br_allowed(self) -> None:
        md = (
            "```mermaid\n"
            "flowchart LR\n"
            "  A[\"<b>Bold</b><br/>Sub\"] --> B\n"
            "```\n"
        )
        errors = self._write_and_validate(md)
        self.assertEqual(len(errors), 1)
        self.assertIn("raw HTML tag", errors[0])

    def test_hardcoded_init_theme_directive_fails(self) -> None:
        md = (
            "```mermaid\n"
            "%%{init: {'theme': 'dark'}}%%\n"
            "flowchart LR\n"
            "  A --> B\n"
            "```\n"
        )
        errors = self._write_and_validate(md)
        self.assertEqual(len(errors), 1)
        self.assertIn("theme directive", errors[0])

    def test_unbalanced_subgraph_fails(self) -> None:
        unclosed = "```mermaid\nflowchart LR\n  subgraph S\n    A --> B\n```\n"
        errors_unclosed = self._write_and_validate(unclosed)
        self.assertEqual(len(errors_unclosed), 1)
        self.assertIn("unclosed 'subgraph'", errors_unclosed[0])

        extra_end = "```mermaid\nflowchart LR\n  A --> B\n  end\n```\n"
        errors_extra = self._write_and_validate(extra_end)
        self.assertEqual(len(errors_extra), 1)
        self.assertIn("unmatched 'end'", errors_extra[0])

    def test_cli_main_exit_codes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            good = root / "docs" / "good.md"
            good.parent.mkdir(parents=True, exist_ok=True)
            good.write_text("```mermaid\nflowchart TD\n  A --> B\n```\n", encoding="utf-8")
            self.assertEqual(checker.main(["--root", str(root)]), 0)

            bad = root / "docs" / "bad.md"
            bad.write_text(
                "```mermaid\nflowchart TD\n  style A fill:#eee\n```\n",
                encoding="utf-8",
            )
            self.assertEqual(checker.main(["--root", str(root)]), 1)


if __name__ == "__main__":
    unittest.main()
