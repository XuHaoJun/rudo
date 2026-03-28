# [Bug]: scan_page_for_unmarked_refs uses unsafe set_mark instead of atomic try_mark

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires specific timing: slot swept and reused with matching generations between set_mark and is_allocated check |
| **Severity (嚴重程度)** | Critical | Can cause mark bitmap corruption, leading to premature collection of live objects |
| **Reproducibility (復現難度)** | Very High | Needs precise timing of lazy sweep + generation wraparound edge case |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `scan_page_for_unmarked_refs` in `crates/rudo-gc/src/gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`scan_page_for_unmarked_refs` should use atomic CAS (`try_mark`) to safely mark slots, similar to `scan_page_for_marked_refs`. The mark should only be considered valid after all safety checks pass.

### 實際行為 (Actual Behavior)
`scan_page_for_unmarked_refs` (line 983) uses `set_mark(i)` which unconditionally sets the mark bit via `fetch_or`, then performs validation checks afterwards. If validation fails and generations coincidentally match, `clear_mark_atomic(i)` corrupts the new object's metadata.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The function `scan_page_for_unmarked_refs` uses a "mark first, check later" pattern:

1. Line 983: `(*header).set_mark(i)` - unconditionally sets mark bit via `fetch_or`
2. Line 991: `is_allocated(i)` check - happens AFTER mark is already set
3. Lines 994-998: Generation check to detect slot reuse
4. If generations match: `clear_mark_atomic(i)` - corrupts new object's metadata

Compare with `scan_page_for_marked_refs` (line 836) which uses `try_mark`:
- CAS only sets bit if current value matches expected
- Returns `Err(())` on concurrent modification
- No need to "undo" mark after validation failure

The semantic difference:
- `set_mark`: `fetch_or` - always sets, returns old value
- `try_mark`: `compare_exchange` - only sets if CAS succeeds

When generations coincidentally match (e.g., generation wraparound after ~4 billion cycles), the code incorrectly clears the mark of a valid live object.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Theoretical race condition - extremely difficult to reproduce reliably:
1. Thread A calls `scan_page_for_unmarked_refs` on page P, slot i
2. `set_mark(i)` succeeds - mark bit is now set
3. Thread B sweeps slot i and reallocates to new object with same generation
4. Thread A's `is_allocated(i)` check passes (slot is now allocated)
5. Thread A's `generation() == marked_generation` check passes (wraparound coincidence)
6. Thread A calls `clear_mark_atomic(i)` - corrupts new object's mark!

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Replace `set_mark` with `try_mark` in `scan_page_for_unmarked_refs`, similar to how `scan_page_for_marked_refs` is implemented:

```rust
// Instead of:
if (*header).set_mark(i) {
    // validation checks...
}

// Use CAS loop like scan_page_for_marked_refs:
loop {
    match (*header).try_mark(i) {
        Ok(false) => break, // already marked
        Ok(true) => {
            // validation checks...
            break;
        }
        Err(()) => {} // CAS failed, retry
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The "mark first, check later" pattern is fundamentally unsafe in a concurrent GC. Even with generation checks, there's an edge case where generation wraparound causes false matches. The correct approach is to use CAS which provides atomicity between the mark and the check. The fact that `scan_page_for_marked_refs` uses `try_mark` correctly while `scan_page_for_unmarked_refs` uses `set_mark` suggests this was an oversight during incremental marking implementation.

**Rustacean (Soundness 觀點):**
This is a soundness issue. Premature collection due to corrupted mark bitmap is undefined behavior - the program can use freed memory. The `&mut self` requirement on `set_mark` vs `&self` on `try_mark` also hints at the design intent: `set_mark` is meant for single-threaded contexts while `try_mark` is for concurrent marking.

**Geohot (Exploit 觀點):**
While extremely difficult to trigger, this is a potential exploitation vector. If an attacker can influence GC timing through allocations, they might be able to trigger the race condition and cause mark bitmap corruption that leads to use-after-free. The generation wraparound requirement (~4 billion cycles) is high but not impossible with persistent allocations.

(End of file - total 107 lines)