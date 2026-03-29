# [Bug]: cell.rs incremental_write_barrier large object path has_gen_old TOCTOU

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep and write barrier on large object |
| **Severity (嚴重程度)** | `High` | Could cause barrier to be skipped, leading to premature collection |
| **Reproducibility (復現難度)** | `High` | Requires precise timing of slot reuse during lazy sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::incremental_write_barrier` (cell.rs large object path)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.19`

---

## 📝 問題描述 (Description)

In `cell.rs` `incremental_write_barrier`, the **large object path** has a TOCTOU race where `has_gen_old` is read BEFORE the second `is_allocated` check. This allows the early exit condition to fire incorrectly when a slot is swept and reused between the checks.

### 預期行為 (Expected Behavior)
The barrier should verify slot is still allocated before reading `has_gen_old` and should not early-exit if the slot was reused.

### 實際行為 (Actual Behavior)
The second `is_allocated(0)` check at line 1303 occurs AFTER the early exit condition at lines 1300-1301. If the slot is swept and reused between the first `is_allocated(0)` (line 1295) and reading `has_gen_old` (line 1299), the new object in the reused slot may have `generation == 0` and `has_gen_old == false`, causing the early exit to fire when it shouldn't.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Buggy code in cell.rs `incremental_write_barrier` large object path (lines 1294-1306):**

```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot.
if !(*h_ptr).is_allocated(0) {      // LINE 1295 - FIRST CHECK
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();   // LINE 1299 - READ has_gen_old
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {  // LINE 1300
    return;                        // LINE 1301 - EARLY EXIT (BUG!)
}
if !(*h_ptr).is_allocated(0) {      // LINE 1303 - SECOND CHECK (TOO LATE!)
    return;
}
```

**Compare to heap.rs `incremental_write_barrier` (which was fixed in e320eb5):**

```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot (bug286).
if !(*h_ptr).is_allocated(0) {      // FIRST CHECK
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
// Second is_allocated check BEFORE reading has_gen_old to fix TOCTOU (bug457).
// Must verify slot is still allocated before reading any GcBox fields.
if !(*h_ptr).is_allocated(0) {      // SECOND CHECK - BEFORE has_gen_old read
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();   // NOW SAFE TO READ
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
```

**The race scenario:**
1. Thread A: First `is_allocated(0)` passes at line 1295
2. Thread B: Lazy sweep deallocates the slot, marks it as free
3. Thread C: New allocation reuses the slot with fresh GcBox (generation=0, GEN_OLD_FLAG=false)
4. Thread A: Reads `has_gen_old` at line 1299 from the new object (value=false)
5. Thread A: Checks `generation == 0 && !has_gen_old` → TRUE
6. Thread A: Returns early at line 1301, **skipping the barrier**
7. Thread A: Never reaches the second `is_allocated(0)` check at line 1303

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable incremental marking and lazy sweep features
2. Allocate a large object in old generation
3. Create a cross-generational reference (OLD → YOUNG) via `GcCell::borrow_mut`
4. Trigger lazy sweep to reclaim the large object slot concurrently
5. Allocate a new object in the reused slot with `generation == 0`
6. The barrier for the OLD→YOUNG reference will incorrectly early-exit

**Note:** This is a race condition that requires precise timing. The non-large-object path has a similar issue where the second `is_allocated` check comes after the early exit condition.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Apply the same fix that was done in heap.rs (commit e320eb5) to the cell.rs `incremental_write_barrier` large object path:

Move the second `is_allocated(0)` check to BEFORE reading `has_gen_old`:

```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot.
if !(*h_ptr).is_allocated(0) {
    return;
}
// FIX: Second is_allocated check BEFORE reading has_gen_old
if !(*h_ptr).is_allocated(0) {
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
```

Additionally, consider fixing the non-large-object path in `incremental_write_barrier` and both paths in `generational_write_barrier` which have similar patterns.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The TOCTOU in incremental_write_barrier is particularly dangerous because it can cause SATB violations. When a generational reference from an old object to a young object is incorrectly skipped, the young object may be collected prematurely during minor GC. The incremental marking relies on write barriers to maintain correctness; skipping them breaks the invariants.

**Rustacean (Soundness 觀點):**
This is a classic TOCTOU (Time-Of-Check-Time-Of-Use) bug. The slot can be deallocated and reused between the first is_allocated check and reading has_gen_old. When the slot is reused, the new object may have different generation and flag values, leading to incorrect early exit. This is technically undefined behavior in the sense that we're reading from an object that may have been freed and replaced.

**Geohot (Exploit 觀點):**
If an attacker can influence allocation patterns (e.g., via incremental marking fallback causing emergency allocations), they might be able to trigger this race more reliably. The result would be premature collection of objects that are still reachable, which could be leveraged in certain GC timing attacks or to create use-after-free conditions in combination with other bugs.

(End of file - total 130 lines)