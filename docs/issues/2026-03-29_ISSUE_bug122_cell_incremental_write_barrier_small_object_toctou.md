# [Bug]: cell.rs incremental_write_barrier small object path has_gen_old TOCTOU

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep and write barrier on small object |
| **Severity (嚴重程度)** | `High` | Could cause barrier to be skipped, leading to premature collection |
| **Reproducibility (復現難度)** | `High` | Requires precise timing of slot reuse during lazy sweep |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::incremental_write_barrier` (cell.rs small object path)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.19`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
The barrier should verify slot is still allocated before reading `has_gen_old` and should not early-exit if the slot was reused.

### 實際行為 (Actual Behavior)
In `cell.rs` `incremental_write_barrier`, the **small object path** has a TOCTOU race where `has_gen_old` is read at line 1335 BEFORE the second `is_allocated` check at line 1340. The early exit condition at lines 1336-1337 fires based on `has_gen_old` from a potentially reused slot.

This is the same pattern as bug459 (large object path) but for the small object path. Bug459 line 94 explicitly notes: "The non-large-object path has a similar issue where the second `is_allocated` check comes after the early exit condition."

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Buggy code in cell.rs `incremental_write_barrier` small object path (lines 1329-1343):**

```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot.
if !(*h.as_ptr()).is_allocated(index) {  // LINE 1330 - FIRST CHECK
    return;
}
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // LINE 1335 - READ has_gen_old
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {  // LINE 1336
    return;  // LINE 1337 - EARLY EXIT (BUG!)
}
// Second is_allocated check - prevents TOCTOU race (bug376)
if !(*h.as_ptr()).is_allocated(index) {  // LINE 1340 - AFTER early exit (TOO LATE!)
    return;
}
```

**Compare to heap.rs `incremental_write_barrier` (which was fixed in e320eb5):**

```rust
if !(*h_ptr).is_allocated(0) {      // FIRST CHECK
    return;
}
if !(*h_ptr).is_allocated(0) {      // SECOND CHECK - BEFORE has_gen_old read
    return;
}
let has_gen_old = (*gc_box_addr).has_gen_old_flag();   // NOW SAFE TO READ
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
```

**The race scenario:**
1. Thread A: First `is_allocated(index)` passes at line 1330
2. Thread B: Lazy sweep deallocates the slot, marks it as free
3. Thread C: New allocation reuses the slot with fresh GcBox (generation=0, GEN_OLD_FLAG=false)
4. Thread A: Reads `has_gen_old` at line 1335 from the new object (value=false)
5. Thread A: Checks `generation == 0 && !has_gen_old` → TRUE
6. Thread A: Returns early at line 1337, **skipping the barrier**
7. Thread A: Never reaches the second `is_allocated(index)` check at line 1340

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable incremental marking and lazy sweep features
2. Allocate a small object in old generation
3. Create a cross-generational reference (OLD → YOUNG) via `GcCell::borrow_mut`
4. Trigger lazy sweep to reclaim the small object slot concurrently
5. Allocate a new object in the reused slot with `generation == 0`
6. The barrier for the OLD→YOUNG reference will incorrectly early-exit

**Note:** This is a race condition that requires precise timing. Similar to bug459 but for small object path.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Apply the same fix that was done in heap.rs (commit e320eb5) to the cell.rs `incremental_write_barrier` small object path:

Move the second `is_allocated(index)` check to BEFORE reading `has_gen_old`:

```rust
// Skip if slot was swept; avoids corrupting remembered set with reused slot.
if !(*h.as_ptr()).is_allocated(index) {  // FIRST CHECK
    return;
}
// FIX: Second is_allocated check BEFORE reading has_gen_old
if !(*h.as_ptr()).is_allocated(index) {  // SECOND CHECK - BEFORE has_gen_old read
    return;
}
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// No third check needed - we've already validated slot is allocated
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The TOCTOU in incremental_write_barrier is particularly dangerous because it can cause SATB violations. When a generational reference from an old object to a young object is incorrectly skipped, the young object may be collected prematurely during minor GC. The incremental marking relies on write barriers to maintain correctness; skipping them breaks the invariants.

**Rustacean (Soundness 觀點):**
This is a classic TOCTOU (Time-Of-Check-Time-Of-Use) bug. The slot can be deallocated and reused between the first is_allocated check and reading has_gen_old. When the slot is reused, the new object may have different generation and flag values, leading to incorrect early exit. This is technically undefined behavior in the sense that we're reading from an object that may have been freed and replaced.

**Geohot (Exploit 觀點):**
If an attacker can influence allocation patterns (e.g., via incremental marking fallback causing emergency allocations), they might be able to trigger this race more reliably. The result would be premature collection of objects that are still reachable, which could be leveraged in certain GC timing attacks or to create use-after-free conditions in combination with other bugs.

---

## 相關 Bug

- bug459: Same issue in large object path (cell.rs `incremental_write_barrier` large object path) - Open
- bug457: Same issue in heap.rs `incremental_write_barrier` large object path - Fixed
- bug376: GcThreadSafeCell barrier TOCTOU - related but different function
- bug282: Similar issue in heap.rs `incremental_write_barrier` - Invalid (but bug457 is the fix for that)

---

## 修復記錄

Not yet fixed.