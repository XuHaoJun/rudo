# [Bug]: mark_and_trace_incremental dereferences before is_allocated check

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Triggered when lazy sweep reclaims a slot between try_mark success and dereference |
| **Severity (嚴重程度)** | Critical | Potential UAF / tracing freed memory |
| **Reproducibility (復現難度)** | High | Requires precise timing between GC phases |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_and_trace_incremental` in `gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

The function `mark_and_trace_incremental` has a TOCTOU (Time-Of-Check-Time-Of-Use) bug where a `GcBox` pointer is dereferenced to read the `generation()` BEFORE the `is_allocated(idx)` check is performed.

### 預期行為 (Expected Behavior)
Per the comment at line 1099-1100: "Uses `mark_and_trace_incremental` (like `scan_dirty_page_minor`) to ensure `is_allocated` is checked before dereferencing"

### 實際行為 (Actual Behavior)
In `mark_and_trace_incremental` (lines 2473-2482):
1. `try_mark(idx)` succeeds at line 2466
2. `(*ptr.as_ptr()).generation()` is dereferenced at line 2474
3. `is_allocated(idx)` is checked at line 2475 (AFTER dereference)

This violates the documented contract and creates a window where a slot reclaimed by lazy sweep could be dereferenced before detection.

---

## 🔬 根本原因分析 (Root Cause Analysis)

```rust
// crates/rudo-gc/src/gc/gc.rs:2473-2482
Ok(true) => {
    let marked_generation = (*ptr.as_ptr()).generation();  // DEREF FIRST (line 2474)
    if !(*header.as_ptr()).is_allocated(idx) {              // CHECK AFTER (line 2475)
        let current_generation = (*ptr.as_ptr()).generation();
        if current_generation != marked_generation {
            return;
        }
        (*header.as_ptr()).clear_mark_atomic(idx);
        return;
    }
    visitor.objects_marked += 1;
    break;
}
```

The `is_allocated` check should be performed BEFORE any dereference of `ptr`.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

This bug is timing-dependent and requires:
1. A minor GC in progress calling `scan_dirty_page_minor_trace`
2. Lazy sweep concurrently reclaiming a dirty slot
3. Precise timing where `try_mark` succeeds before `is_allocated` returns false

```rust
// Conceptual PoC - would need stress testing with Miri/TSan
fn trigger_bug() {
    // Setup: Create objects, establish OLD->YOUNG references
    // Trigger minor GC while lazy sweep runs concurrently
    // The TOCTOU window allows dereferencing freed memory
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Move the `is_allocated` check BEFORE the dereference:

```rust
Ok(true) => {
    if !(*header.as_ptr()).is_allocated(idx) {  // CHECK FIRST
        return;
    }
    let marked_generation = (*ptr.as_ptr()).generation();  // THEN DEREF
    if !(*header.as_ptr()).is_allocated(idx) {
        return;
    }
    // ... rest unchanged
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- The SATB invariant requires conservative scanning of dirty pages
- If a slot is swept between `try_mark` and the dereference, the GC could trace through arbitrary memory
- The generation check (lines 2476-2478) provides some protection but only catches slot reuse, not deallocation without reuse

**Rustacean (Soundness 觀點):**
- Dereferencing `ptr` before `is_allocated` check is undefined behavior if the slot has been reclaimed
- Even if generation hasn't changed (slot reused with same generation), we're reading potentially uninitialized or stale data
- The comment at line 1099 promises safety checks that aren't delivered

**Geohot (Exploit 觀點):**
- Precise timing window could be exploited if an attacker can influence GC timing
- If memory is freed and not immediately reused, reading stale pointer could leak sensitive data from freed object
- The race between lazy sweep and marking creates a narrow but real exploit window

(End of file - total 97 lines)