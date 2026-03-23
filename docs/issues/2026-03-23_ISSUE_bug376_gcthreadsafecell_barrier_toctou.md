# [Bug]: GcThreadSafeCell barriers missing second is_allocated check - TOCTOU race

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires concurrent sweep and mutator write barrier |
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

After reading `has_gen_old` flag from a slot, there should be a second `is_allocated` check before modifying the dirty bitmap or recording in the remembered buffer. This pattern exists in `incremental_write_barrier` in `heap.rs` (lines 3198-3201, bug364 fix).

### 實際行為 (Actual Behavior)

`GcThreadSafeCell` barrier functions only perform one `is_allocated` check BEFORE reading `has_gen_old`, then proceed directly to `set_dirty`/`record_in_remembered_buffer` with no re-check.

### 程式碼位置

**cell.rs:1271-1284** - `GcThreadSafeCell::incremental_write_barrier` small object path:
```rust
// Line 1272: First is_allocated check
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
// Lines 1275-1280: Read has_gen_old
let gc_box_addr = (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// Line 1284: NO SECOND is_allocated check!
heap.record_in_remembered_buffer(header);
```

**cell.rs:1340-1357** - `GcThreadSafeCell::generational_write_barrier` small object path:
```rust
// Line 1342: First is_allocated check
if !(*header.as_ptr()).is_allocated(index) {
    return;
}
// Lines 1347-1354: Read has_gen_old
let gc_box_addr = (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*header.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// Lines 1355-1356: NO SECOND is_allocated check!
(*header.as_ptr()).set_dirty(index);
heap.add_to_dirty_pages(header);
```

**Correct pattern in heap.rs:3198-3203:**
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
(h, index)
};

// Second is_allocated check - prevents TOCTOU (bug364)
if !(*header.as_ptr()).is_allocated(index) {
    return;
}

heap.record_in_remembered_buffer(header);
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

TOCTOU (Time-Of-Check-Time-OF-Use) race condition:

1. Thread A (mutator): `GcThreadSafeCell::incremental_write_barrier` passes `is_allocated(index)` check at line 1272 (slot is allocated with OLD object)
2. Thread B (GC): Sweeps the slot (slot becomes unallocated)
3. Thread B: Reuses slot for NEW object with `has_gen_old = false`
4. Thread A: Reads `has_gen_old = false` from NEW object's gc_box_addr
5. Thread A: If page `generation > 0`, doesn't return early, proceeds to line 1284
6. Thread A: Records page in remembered buffer based on stale/incorrect state

The same race exists in `generational_write_barrier` (lines 1340-1357).

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires ThreadSanitizer to detect data race
// RUSTFLAGS="-Z sanitizer=thread" cargo test

// Or concurrent stress test with many threads triggering:
// 1. Allocations that fill up pages
// 2. Concurrent minor GCs (lazy sweep)
// 3. Write barriers on GcThreadSafeCell
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add second `is_allocated` check in both functions, matching the pattern in `heap.rs`:

**For `incremental_write_barrier` (cell.rs lines 1280-1284):**
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// Second is_allocated check - prevents TOCTOU race (bug376)
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
heap.record_in_remembered_buffer(header);
```

**For `generational_write_barrier` (cell.rs lines 1354-1357):**
```rust
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*header.as_ptr()).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// Second is_allocated check - prevents TOCTOU race (bug376)
if !(*header.as_ptr()).is_allocated(index) {
    return;
}
(*header.as_ptr()).set_dirty(index);
heap.add_to_dirty_pages(header);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The TOCTOU race in write barriers is subtle but serious. During lazy sweep, a slot can be deallocated and reused between the `is_allocated` check and the barrier write. This can cause the remembered set to contain stale entries from the OLD object while the NEW object has different `has_gen_old` state. This corrupts the GC's view of which pages need scanning.

**Rustacean (Soundness 觀點):**
While this is not a direct data race on a single field (the `is_allocated` check is a logical check), the resulting behavior is as if the barrier fired on the wrong object state. The compiler could optimize this in surprising ways since the `is_allocated` check appears to be just a conditional return.

**Geohot (Exploit 攻擊觀點):**
If an attacker can influence allocation patterns to trigger concurrent sweep during a critical write barrier, they could potentially cause the remembered set to include pages with incorrect state. Combined with other GC bugs, this could lead to use-after-free. However, the timing requirements make this difficult to exploit reliably.