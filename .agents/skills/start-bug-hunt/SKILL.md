---
name: start-bug-hunt
description: Structured bug hunting for rudo-gc using multi-perspective analysis. Simulates R. Kent Dybvig (GC/memory), Rustacean (soundness/UB), and George Hotz (exploits/race conditions). Use when hunting bugs, debugging GC issues, when the user invokes /start-bug-hunt, or asks to find program bugs (尋找程式 bug).
---

# Start Bug Hunt

Structured bug hunting workflow with parallel-world expert collaboration. Output goes to `docs/issues/`.

## Personas

| Expert | Perspective |
| :--- | :--- |
| **R. Kent Dybvig** | Chez Scheme author; GC, memory layout, and allocator precision. |
| **Rustacean** (Rust Leadership Council) | Memory safety, soundness, UB sensitivity, Send/Sync correctness. |
| **George Hotz** (Geohot) | Exploit mindset; system boundaries, race conditions, fragile low-level mechanisms. |

## Workflow

### Step 1: Find the Bug

- Analyze code, reproduce failure, or trace from symptoms.
- Formulate clear expected vs actual behavior.

### Step 2: Check for Duplicates

- Search `docs/issues/` and `docs/history-issues/` for related issues.
- **If duplicate found**: Update existing issue with new findings (PoC, root cause, fix ideas). Do not create a new file.

### Step 3: Create Issue File (if no duplicate)

- Ensure `docs/issues/` exists.
- Create file: `docs/issues/YYYY-MM-DD_ISSUES_<bug-name>.md` (use today's date).
- Use the template in [ISSUE_TEMPLATE.md](ISSUE_TEMPLATE.md).

## Output Format

Use the full template in [ISSUE_TEMPLATE.md](ISSUE_TEMPLATE.md). Essential sections:

1. **Threat Model** – Likelihood, Severity, Reproducibility table  
2. **Affected Component** – Component, OS, Rust/rudo-gc versions  
3. **Description** – Expected vs Actual behavior  
4. **Root Cause Analysis** – Technical details  
5. **PoC** – Minimal reproducible code  
6. **Suggested Fix** – Remediation steps  
7. **Internal Discussion Record** – Per-persona analysis (R. Kent Dybvig, Rustacean, Geohot)

## Persona Discussion Guidelines

Simulate a short parallel discussion. Each persona contributes from their perspective:

- **R. Kent Dybvig**: GC design, memory layout, write barriers, incremental marking, allocator behavior.
- **Rustacean**: UB risks, unsafe usage, Send/Sync, lifetime/borrow soundness.
- **Geohot**: Exploit paths, race conditions, edge cases, “clever but brittle” mechanisms.

Then summarize the consensus or main conclusions.
