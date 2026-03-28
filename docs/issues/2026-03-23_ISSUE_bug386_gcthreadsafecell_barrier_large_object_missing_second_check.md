# [Bug]: GcThreadSafeCell barrier large object path missing second is_allocated check - TOCTOU race

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent lazy sweep and large object write barrier |
| **Severity (嚴重程度)** | `Critical` | TOCTOU race can corrupt remembered set, potentially causing UAF |
| **Reproducibility (復現難度)** | `High` | Race condition requires ThreadSanitizer or concurrent stress test |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `GcThreadSafeCell::incremental_write_barrier`, `GcThreadSafeCell::generational_write_barrier`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `0.8.x`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

After reading `has_gen_old` flag from a slot, there should be a second `is_allocated` check before modifying the dirty bitmap or recording in the remembered buffer. This pattern exists in the normal (non-large-object) path (bug376 fix).

### 實際行為 (Actual Behavior)

The **large object path** in both barrier functions only performs one `is_allocated` check BEFORE reading `has_gen_old`, then proceeds directly to barrier writes with NO re-check. This is inconsistent with the normal path which has the second check.

### 程式碼位置

**cell.rs:1241-1250** - `GcThreadSafeCell::incremental_write_barrier` large object path:
```rust
// Line 1242: First is_allocated check
if !(*h_ptr).is_allocated(0) {
    return;
}
// Lines 1245-1248: Read has_gen_old
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// Line 1250: NO SECOND is_allocated check! - BUG!
NonNull::new_unchecked(h_ptr)
```

**cell.rs:1316-1328** - `GcThreadSafeCell::generational_write_barrier` large object path:
```rust
// Line 1317: First is_allocated check
if !(*header).is_allocated(0) {
    return;
}
// Lines 1322-1325: Read has_gen_old
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*header).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// Lines 1327-1328: NO SECOND is_allocated check! - BUG!
(*header).set_dirty(0);
heap.add_to_dirty_pages(NonNull::new_unchecked(header));
```

**Correct pattern (normal path - bug376 fix) at lines 1281-1284:**
```rust
// Second is_allocated check - prevents TOCTOU race (bug376)
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

TOCTOU (Time-Of-Check-Time-Of-Use) race condition in large object path:

1. Thread A (mutator): `GcThreadSafeCell::incremental_write_barrier` large object path passes `is_allocated(0)` check at line 1242 (slot is allocated with OLD object)
2. Thread B (GC): Sweeps the large object slot (slot becomes unallocated)
3. Thread B: Reuses slot for NEW object with `has_gen_old = false`
4. Thread A: Reads `has_gen_old = false` from NEW object's gc_box_addr
5. Thread A: If page `generation > 0`, doesn't return early, proceeds to line 1288
6. Thread A: Records page in remembered buffer based on stale/incorrect state

The same race exists in `generational_write_barrier` large object path (lines 1316-1328).

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires ThreadSanitizer to detect data race
// RUSTFLAGS="-Z sanitizer=thread" cargo test

// Or concurrent stress test with many threads triggering:
// 1. Large object allocations
// 2. Concurrent minor GCs (lazy sweep)
// 3. Write barriers on GcThreadSafeCell with large objects
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add second `is_allocated` check in both large object paths, matching the pattern in the normal path:

**For `incremental_write_barrier` large object path (cell.rs lines 1248-1250):**
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// Second is_allocated check - prevents TOCTOU race (bug376 fix incomplete)
if !(*h_ptr).is_allocated(0) {
    return;
}
NonNull::new_unchecked(h_ptr)
```

**For `generational_write_barrier` large object path (cell.rs lines 1325-1328):**
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*header).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// Second is_allocated check - prevents TOCTOU race (bug376 fix incomplete)
if !(*header).is_allocated(0) {
    return;
}
(*header).set_dirty(0);
heap.add_to_dirty_pages(NonNull::new_unchecked(header));
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The TOCTOU race in write barriers for large objects is the same as for normal objects - during lazy sweep, a slot can be deallocated and reused between the `is_allocated` check and the barrier write. Large objects are particularly vulnerable because they have a single slot (index 0) but span multiple pages.

**Rustacean (Soundness 觀點):**
This is a soundness bug. The second `is_allocated` check that was added for the normal path (bug376) was not added for the large object path, creating an inconsistent implementation. The compiler could optimize this in surprising ways.

**Geohot (Exploit 攻擊觀點):**
Large objects with interior pointers are already complex (see bug190). The missing second check in the large object barrier path adds another exploitation vector. If an attacker can trigger lazy sweep on a large object during a write barrier, they could corrupt the remembered set.

---

## 🔗 相關 Issue

- bug376: GcThreadSafeCell barriers missing second is_allocated check (fixed for normal path only, large object path incomplete)

---

## Resolution (2026-03-28)

**Verified fixed in code:** `GcThreadSafeCell::incremental_write_barrier` and `generational_write_barrier` large-object branches now match the normal-path bug376 pattern: after `has_gen_old` / generation early-exit logic, a second `is_allocated(0)` runs before `record_in_remembered_buffer` / `set_dirty` (see `cell.rs` large-object arms). No further code change required.