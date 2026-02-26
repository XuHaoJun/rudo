---
name: issue-resolve
description: "Resolve rudo-gc issues tracked in docs/issues/. Use when the user asks to fix, investigate, triage, verify, or close a bug listed in docs/issues/, or when asked to update issue status / ISSUES_REPORT.md. Handles: (1) Investigating Open issues by reading codebase, (2) Applying code fixes for confirmed bugs, (3) Updating issue Status and Tags, (4) Keeping ISSUES_REPORT.md in sync, (5) Marking Invalid issues without modifying code."
---

# Issue Resolve

Workflow for investigating and resolving rudo-gc issues from `docs/issues/`.

## Issue File Format

Issues live in `docs/issues/YYYY-MM-DD_ISSUE_bug<N>_<short-desc>.md`.  
Fields: `Status` (Open / Fixed / Invalid) and `Tags` (Verified / Not Verified / Not Reproduced).  
Template: [assets/ISSUE_TEMPLATE.md](assets/ISSUE_TEMPLATE.md)

## Workflow

### Step 0: Select the Target Issue & Snapshot State

Run this step **before** making any changes.

#### 0a — Select the issue to work on

If the user did **not** specify which issue to handle, automatically pick the **first** issue with `Status: Open` (lowest bug number):

```bash
# List all Open issues sorted by bug number
grep -l 'Status.*Open' docs/issues/*.md \
  | grep -oP 'bug\K\d+' \
  | sort -n \
  | head -1
```

Then read that file: `docs/issues/YYYY-MM-DD_ISSUE_bugN_<short-desc>.md`.

#### 0b — Regenerate ISSUES_REPORT.md (initial snapshot)

```bash
python3 .agents/skills/issue-resolve/scripts/generate_issues_report.py
```

This captures the **baseline state** before any edits.

---

### Step 1: Read the Issue

- Read the target issue file in `docs/issues/`.
- Identify: affected component(s), root cause claim, suggested fix, current Status/Tags.

### Step 2: Investigate the Codebase

- Locate the relevant source files (`crates/rudo-gc/src/`).
- Verify whether the root cause described in the issue actually exists in the current code.
- Check related tests in `crates/rudo-gc/tests/`.

### Step 2.5: Reproduction Check (optional but strongly preferred)

This step determines whether the bug is real **before** writing any fix.

#### 2.5a — Check Existing Test Coverage

Search the test directory for tests that exercise the affected code path:

```bash
grep -r "affected_fn_name\|related_keyword" crates/rudo-gc/tests/
```

**Outcomes:**

| Finding | Action |
|---|---|
| An existing test already **fails** on the current code and matches the issue | Bug confirmed — proceed to Step 3 |
| An existing test already **passes** and would have caught this bug if real | Likely Invalid — run the test to confirm, then classify |
| No relevant test exists | Write a minimal reproduction test (see 2.5b) |

> If a passing test would definitively have caught the reported bug, you may classify the issue as `Invalid` without a code fix. Run the specific test first to be certain.

#### 2.5b — Run a Targeted Test

Run the single most relevant test (or write a minimal one) to confirm reproducibility:

```bash
# Run a single existing test
cargo test -p rudo-gc test_name -- --test-threads=1

# Run all tests in a specific test file
cargo test -p rudo-gc --test file_name -- --test-threads=1
```

**Interpreting results:**

| Result | Interpretation |
|---|---|
| Test **fails** (panic / assert / UB) | Bug confirmed → proceed to Step 3 |
| Test **passes** when it should fail | Bug not reproducible with current code — check if already fixed |
| Test **passes** and this is the correct coverage | Issue is likely **Invalid** or already fixed |

> For race conditions and SATB barrier bugs, a single-threaded test passing does **not** rule out the bug. Note this in the issue and keep `Status: Open`.

#### 2.5c — Write a Minimal Reproduction Test (if needed)

If no test covers the exact scenario, write the smallest possible test before fixing:

```rust
#[test]
fn repro_bug_N_short_description() {
    rudo_gc::init();
    // Setup: create the object state that triggers the bug
    // ...
    // Trigger: call the function described in the issue
    // ...
    // Assert: verify the expected safe behaviour
    // ...
}
```

Add this test to the most appropriate file in `crates/rudo-gc/tests/`. The test should **fail** on the buggy code and **pass** after the fix is applied.

### Step 3: Classify the Issue

| Finding | Action |
|---|---|
| Bug confirmed (code path verified + test reproduces) | Proceed to Step 4 |
| Bug real but needs more info | Update issue notes, keep `Status: Open` |
| Issue is misidentified (test passes, code path doesn't exist) | Set `Status: Invalid`, explain why, STOP |

> See [references/resolve-patterns.md](references/resolve-patterns.md) for common issue types and their fix/classification patterns.

### Step 4: Apply the Fix

- Make minimal, targeted changes. Do not refactor surrounding code.
- Follow existing code style. Run `cargo fmt` if needed, but avoid unrelated cleanups.
- The reproduction test from Step 2.5c (if written) must pass after the fix.

### Step 5: Verify

Run the reproduction test and the full test suite to check for regressions:

```bash
# Verify the specific fix
cargo test -p rudo-gc test_name -- --test-threads=1

# Run the full suite
bash test.sh
```

If the issue involves a race condition or SATB barrier, see [references/resolve-patterns.md](references/resolve-patterns.md) for the correct test strategy (minor GC, TSan, etc.).

### Step 6: Update the Issue File

Update the issue's `Status` and `Tags` fields:

| Outcome | Status | Tags |
|---|---|---|
| Fixed and verified | `Fixed` | `Verified` |
| Fixed, verification inconclusive | `Fixed` | `Not Verified` |
| Confirmed but unfixed | `Open` | `Verified` |
| Cannot reproduce | `Open` | `Not Reproduced` |
| Misidentified | `Invalid` | keep existing tag |

Append a brief resolution note below the existing content if adding new findings.

### Step 7: Regenerate ISSUES_REPORT.md

After updating the issue file's `Status` / `Tags`, regenerate the report with the script (do **not** edit it by hand):

```bash
python3 .agents/skills/issue-resolve/scripts/generate_issues_report.py
```

The script automatically:
- Recalculates all statistics (Fixed / Open / Invalid counts, Tags counts)
- Rebuilds the full issue table sorted by bug number
- Overwrites `docs/issues/ISSUES_REPORT.md`

> Run the script even if you believe nothing changed — it keeps the report consistent.

## Key Source Locations

| Area | Path |
|---|---|
| GC core | `crates/rudo-gc/src/gc.rs` |
| Write barriers | `crates/rudo-gc/src/barrier.rs` |
| Heap / allocation | `crates/rudo-gc/src/heap.rs` |
| GcCell / thread-safe | `crates/rudo-gc/src/gc_cell.rs`, `gc_thread_safe_cell.rs` |
| GcHandle | `crates/rudo-gc/src/gc_handle.rs` |
| Weak references | `crates/rudo-gc/src/weak.rs` |
| Marker | `crates/rudo-gc/src/marker.rs` |
| Tests | `crates/rudo-gc/tests/` |

## Report Script

| Script | Purpose |
|---|---|
| `scripts/generate_issues_report.py` | Regenerate `docs/issues/ISSUES_REPORT.md` from all issue files |

**Run at Step 0 (snapshot)** and **Step 7 (after status change)**. Never edit `ISSUES_REPORT.md` by hand.

## Important Constraints

- **Default scope** — if the user did not specify an issue, work on the **first `Status: Open` issue** (lowest bug number). Do not ask for confirmation; just pick it and state your choice.
- **Always run the script at the start and end** — Step 0b and Step 7 are mandatory, not optional.
- **Always attempt reproduction before fixing** — Step 2.5 is strongly preferred; skip only for trivially verifiable doc mismatches where the code path is clear from static reading.
- **Existing test coverage can invalidate an issue** — if a directly relevant test already passes and would have caught the bug, classify as `Invalid` after confirming with a test run.
- **Minimal diffs only** — touch only what is needed to fix the bug.
- **Invalid issues** — set `Status: Invalid`, explain, and do NOT modify any source code.
- For fix patterns (SATB, TOCTOU, GcCapture, atomic ordering, doc mismatches): see [references/resolve-patterns.md](references/resolve-patterns.md).
