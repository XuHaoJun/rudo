#!/usr/bin/env python3
"""
generate_issues_report.py
=========================
Regenerate docs/issues/ISSUES_REPORT.md by scanning every issue file in
docs/issues/ and parsing its fixed-format header fields.

Usage (from repo root):
    python3 .agents/skills/issue-resolve/scripts/generate_issues_report.py

Or with an explicit issues directory:
    python3 .agents/skills/issue-resolve/scripts/generate_issues_report.py \
        --issues-dir path/to/docs/issues
"""

from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path


# ---------------------------------------------------------------------------
# Data model
# ---------------------------------------------------------------------------

@dataclass
class Issue:
    filename: str          # e.g. "2026-02-19_ISSUE_bug1_foo.md"
    bug_number: int        # parsed from filename, e.g. 1
    title: str             # parsed from "# [Bug]: <title>"
    status: str            # Open / Fixed / Invalid
    tags: str              # Verified / Not Verified / Not Reproduced


# ---------------------------------------------------------------------------
# Parsing helpers
# ---------------------------------------------------------------------------

_BUG_NUMBER_RE = re.compile(r"_bug(\d+)_", re.IGNORECASE)
_TITLE_RE      = re.compile(r"^#\s*\[Bug\]:\s*(.+)", re.IGNORECASE)
_STATUS_RE     = re.compile(r"^\*\*Status:\*\*\s*(.+)", re.IGNORECASE)
_TAGS_RE       = re.compile(r"^\*\*Tags:\*\*\s*(.+)", re.IGNORECASE)


def parse_issue(path: Path) -> Issue | None:
    """Return an Issue parsed from *path*, or None if the file is not a valid issue."""
    filename = path.name

    # Must match the naming convention
    m = _BUG_NUMBER_RE.search(filename)
    if not m:
        return None

    bug_number = int(m.group(1))

    title  = "<no title>"
    status = "Unknown"
    tags   = "Unknown"

    try:
        text = path.read_text(encoding="utf-8")
    except OSError as exc:
        print(f"[WARN] Cannot read {path}: {exc}", file=sys.stderr)
        return None

    for line in text.splitlines():
        line = line.strip()

        if title == "<no title>":
            m2 = _TITLE_RE.match(line)
            if m2:
                title = m2.group(1).strip()
                continue

        if status == "Unknown":
            m3 = _STATUS_RE.match(line)
            if m3:
                status = m3.group(1).strip()
                continue

        if tags == "Unknown":
            m4 = _TAGS_RE.match(line)
            if m4:
                tags = m4.group(1).strip()
                continue

        # Stop once all three fields are found
        if title != "<no title>" and status != "Unknown" and tags != "Unknown":
            break

    return Issue(
        filename=filename,
        bug_number=bug_number,
        title=title,
        status=status,
        tags=tags,
    )


# ---------------------------------------------------------------------------
# Report generation
# ---------------------------------------------------------------------------

def generate_report(issues: list[Issue]) -> str:
    # ---- statistics --------------------------------------------------------
    status_counts: dict[str, int] = {}
    tags_counts:   dict[str, int] = {}

    for issue in issues:
        status_counts[issue.status] = status_counts.get(issue.status, 0) + 1
        tags_counts[issue.tags]     = tags_counts.get(issue.tags, 0) + 1

    # Canonical ordering for known values
    STATUS_ORDER = ["Fixed", "Open", "Invalid"]
    TAGS_ORDER   = ["Verified", "Not Verified", "Not Reproduced"]

    def _stat_lines(counts: dict[str, int], order: list[str]) -> list[str]:
        lines = []
        # Known values first, then any unexpected values alphabetically
        for key in order:
            if key in counts:
                lines.append(f"- **{key}**: {counts[key]}")
        for key in sorted(counts):
            if key not in order:
                lines.append(f"- **{key}**: {counts[key]}")
        return lines

    stat_status = "\n".join(_stat_lines(status_counts, STATUS_ORDER))
    stat_tags   = "\n".join(_stat_lines(tags_counts,   TAGS_ORDER))

    # ---- table rows --------------------------------------------------------
    rows = []
    for issue in sorted(issues, key=lambda i: i.bug_number):
        link   = f"[{issue.filename}](./{issue.filename})"
        rows.append(
            f"| {link} | {issue.title} | {issue.status} | {issue.tags} |"
        )

    table_body = "\n".join(rows)

    # ---- assemble ----------------------------------------------------------
    report = f"""\
# Bug Issues Report

## Statistics

### By Status
{stat_status}

### By Tags
{stat_tags}

## All Issues

| Issue | Title | Status | Tags |
|---|---|---|---|
{table_body}
"""
    return report


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------

def main() -> None:
    repo_root = Path(__file__).resolve().parents[4]  # .agents/skills/issue-resolve/scripts/ → 4 up
    default_issues_dir = repo_root / "docs" / "issues"

    parser = argparse.ArgumentParser(
        description="Generate docs/issues/ISSUES_REPORT.md from individual issue files."
    )
    parser.add_argument(
        "--issues-dir",
        type=Path,
        default=default_issues_dir,
        help=f"Directory containing issue .md files (default: {default_issues_dir})",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=None,
        help="Output path (default: <issues-dir>/ISSUES_REPORT.md)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print the report to stdout instead of writing to disk.",
    )
    args = parser.parse_args()

    issues_dir: Path = args.issues_dir.resolve()
    if not issues_dir.is_dir():
        print(f"[ERROR] Issues directory not found: {issues_dir}", file=sys.stderr)
        sys.exit(1)

    output_path: Path = (
        args.output.resolve() if args.output
        else issues_dir / "ISSUES_REPORT.md"
    )

    # Parse all issue files (skip ISSUES_REPORT.md and non-issue files)
    issues: list[Issue] = []
    for path in sorted(issues_dir.glob("*.md")):
        if path.name == "ISSUES_REPORT.md":
            continue
        issue = parse_issue(path)
        if issue is not None:
            issues.append(issue)

    if not issues:
        print("[WARN] No issue files found.", file=sys.stderr)

    report = generate_report(issues)

    if args.dry_run:
        print(report)
    else:
        output_path.write_text(report, encoding="utf-8")
        print(f"[OK] Wrote {len(issues)} issues → {output_path}")


if __name__ == "__main__":
    main()
