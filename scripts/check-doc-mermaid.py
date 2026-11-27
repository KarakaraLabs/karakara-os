#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Karakara Labs
#
# SPDX-License-Identifier: Apache-2.0

"""Validate fenced Mermaid diagram blocks for syntax and accessibility guardrails.

Enforces docs/assertions/accessibility.md across README.md, docs/**/*.md, and crates/**/*.md:
- Every ```mermaid fence is properly closed and non-empty.
- First non-comment line declares a supported Mermaid diagram type.
- Flowcharts/graphs have balanced subgraph / end pairs.
- No hardcoded background fill overrides (theme-native fills only for Light/Dark legibility).
- No raw HTML tags or %%{init:...}%% theme overrides inside Mermaid blocks.
"""

from __future__ import annotations

import re
import sys
from dataclasses import dataclass
from pathlib import Path

SUPPORTED_DIAGRAM_HEADERS: tuple[str, ...] = (
    "flowchart",
    "graph",
    "sequenceDiagram",
    "stateDiagram",
    "stateDiagram-v2",
    "classDiagram",
    "erDiagram",
    "xychart-beta",
)

HARDCODED_FILL_RE = re.compile(r"\bfill\s*:\s*(?!none\b)[^\s,;]+", re.IGNORECASE)
# `<br/>` is Mermaid's standard line-break separator; reject styling/layout HTML tags.
HTML_TAG_RE = re.compile(
    r"</?(?:b|i|u|span|div|font|p|strong|em|style|script)\b[^>]*>", re.IGNORECASE
)
INIT_DIRECTIVE_RE = re.compile(r"%%\{.*init.*\}%%", re.IGNORECASE)


@dataclass(frozen=True)
class MermaidBlock:
    path: Path
    start_line: int
    lines: tuple[str, ...]
    closed: bool


def iter_markdown_files(root: Path) -> list[Path]:
    files: list[Path] = []
    readme = root / "README.md"
    if readme.is_file():
        files.append(readme)
    for subdir in ("docs", "crates"):
        base = root / subdir
        if not base.is_dir():
            continue
        for md in sorted(base.rglob("*.md")):
            if "target" in md.parts or "node_modules" in md.parts:
                continue
            files.append(md)
    return files


def extract_mermaid_blocks(path: Path) -> list[MermaidBlock]:
    blocks: list[MermaidBlock] = []
    text = path.read_text(encoding="utf-8")
    raw_lines = text.splitlines()

    in_block = False
    fence_char = "`"
    fence_len = 3
    start_line = 0
    buf: list[str] = []

    for idx, line in enumerate(raw_lines, start=1):
        stripped = line.lstrip()
        if not in_block:
            match = re.match(r"^(`{3,}|~{3,})\s*mermaid\s*$", stripped)
            if match:
                fence = match.group(1)
                fence_char = fence[0]
                fence_len = len(fence)
                in_block = True
                start_line = idx
                buf = []
        else:
            close_re = rf"^{re.escape(fence_char)}{{{fence_len},}}\s*$"
            if re.match(close_re, stripped):
                blocks.append(
                    MermaidBlock(
                        path=path,
                        start_line=start_line,
                        lines=tuple(buf),
                        closed=True,
                    )
                )
                in_block = False
                buf = []
            else:
                buf.append(line)

    if in_block:
        blocks.append(
            MermaidBlock(
                path=path,
                start_line=start_line,
                lines=tuple(buf),
                closed=False,
            )
        )
    return blocks


def validate_block(block: MermaidBlock, root: Path) -> list[str]:
    rel = block.path.relative_to(root)
    errors: list[str] = []

    if not block.closed:
        errors.append(f"{rel}:{block.start_line}: unclosed ```mermaid fence")
        return errors

    non_empty = [
        (block.start_line + offset, line.strip())
        for offset, line in enumerate(block.lines, start=1)
        if line.strip() and not line.strip().startswith("%%")
    ]
    if not non_empty:
        errors.append(f"{rel}:{block.start_line}: empty mermaid block")
        return errors

    header_line_no, header = non_empty[0]
    diagram_kind = header.split()[0]
    if diagram_kind not in SUPPORTED_DIAGRAM_HEADERS:
        errors.append(
            f"{rel}:{header_line_no}: unsupported mermaid diagram type '{diagram_kind}' "
            f"(expected one of: {', '.join(SUPPORTED_DIAGRAM_HEADERS)})"
        )

    is_flowchart = diagram_kind in ("flowchart", "graph")
    subgraph_depth = 0

    for offset, raw_line in enumerate(block.lines, start=1):
        line_no = block.start_line + offset
        stripped = raw_line.strip()

        if INIT_DIRECTIVE_RE.search(stripped):
            errors.append(
                f"{rel}:{line_no}: hardcoded Mermaid %%{{init:...}}%% theme directive "
                "(must use theme-native rendering per docs/assertions/accessibility.md)"
            )

        if stripped.startswith("%%"):
            continue

        if HARDCODED_FILL_RE.search(stripped):
            errors.append(
                f"{rel}:{line_no}: hardcoded fill override '{stripped}' "
                "(use theme-native fills + stroke-only accents per docs/assertions/accessibility.md)"
            )

        if HTML_TAG_RE.search(stripped):
            errors.append(
                f"{rel}:{line_no}: raw HTML tag in Mermaid label '{stripped}' "
                "(avoid HTML tags per docs/assertions/accessibility.md)"
            )

        if is_flowchart:
            if re.match(r"^subgraph\b", stripped):
                subgraph_depth += 1
            elif stripped == "end":
                subgraph_depth -= 1
                if subgraph_depth < 0:
                    errors.append(f"{rel}:{line_no}: unmatched 'end' outside subgraph")

    if is_flowchart and subgraph_depth > 0:
        errors.append(
            f"{rel}:{block.start_line}: {subgraph_depth} unclosed 'subgraph' block(s)"
        )

    return errors


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    md_files = iter_markdown_files(root)
    total_blocks = 0
    all_errors: list[str] = []

    for path in md_files:
        blocks = extract_mermaid_blocks(path)
        total_blocks += len(blocks)
        for block in blocks:
            all_errors.extend(validate_block(block, root))

    if all_errors:
        for err in all_errors:
            print(f"MERMAID ERROR: {err}", file=sys.stderr)
        print(
            f"Mermaid check failed: {len(all_errors)} error(s) across {total_blocks} diagram(s).",
            file=sys.stderr,
        )
        return 1

    print(
        f"Mermaid check passed: {total_blocks} diagram(s) across {len(md_files)} markdown file(s)."
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
