# [Bug]: Slot reuse does not clear is_dropping, causing dropped objects to be skipped in dec_ref

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any slot reuse after an object was in dropping phase |
| **Severity (嚴重程度)** | High | Memory leak - dropped values never have their Drop run |
| **Reproducibility (重現難度)** | Medium | Requires specific object lifecycle with dropping state before reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox` slot reuse, `dec_ref` in `ptr.rs`, `try_pop_from_page` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

When a slot is reused via `alloc_from_free_list` / `try_pop_from_page`, the `is_dropping` field is NOT cleared, unlike other flags (`DEAD_FLAG`, `GEN_OLD_FLAG`, `UNDER_CONSTRUCTION_FLAG`).

### 預期行為 (Expected Behavior)
When a slot is reused, all GC-related flags should be reset so the new object starts with clean state.

### 實際行為 (Actual Behavior)
If a previous object was in the dropping phase (`dropping_state == 1 or 2`) and its slot was swept and reused, the new object inherits `dropping_state != 0`.

When `dec_ref` is called on the new object with `ref_count == 1`:
```rust
// ptr.rs:189
if count == 1 && this.dropping_state() == 0 {
    // Only enters this branch if dropping_state == 0
    if this.try_mark_dropping() {
        unsafe { (this.drop_fn)(self_ptr.cast::<u8>()); }
        // ...
    }
}
```

Since `dropping_state != 0`, the code falls through to line 208-214 which just decrements `ref_count` to 0 WITHOUT calling `drop_fn`. The new object's value is NEVER dropped - a memory leak!

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `heap.rs` at `try_pop_from_page` (lines 2317-2329):

```rust
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_dead();           // DEAD_FLAG cleared ✓
    (*gc_box_ptr).clear_gen_old();        // GEN_OLD_FLAG cleared ✓
    (*gc_box_ptr).clear_under_construction(); // UNDER_CONSTRUCTION cleared ✓
    (*gc_box_ptr).increment_generation(); // generation incremented ✓
    (*header).clear_dirty(idx as usize); // dirty bit cleared ✓
    // BUT: is_dropping is NOT cleared!
}
```

Unlike other flags, `is_dropping` persists across slot reuse. The `init_header_at` function in `ptr.rs` properly initializes `is_dropping` to 0 for new allocations, but this is not called during slot reuse.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This would need to be run with specific conditions:
// 1. Create object A
// 2. Let object A's ref_count go to 1 and enter dropping phase
// 3. Before A is fully dropped, force slot reuse (alloc from free list)
// 4. Create new object B in the reused slot
// 5. Drop B - its value will NOT be dropped because dropping_state inherited from A
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. Add `clear_is_dropping()` method to `GcBox` in `ptr.rs`:
```rust
pub(crate) fn clear_is_dropping(&self) {
    self.is_dropping.store(0, Ordering::Release);
}
```

2. Call `clear_is_dropping()` in `try_pop_from_page` alongside other flag clearing operations.

Alternatively, call `init_header_at(gc_box_ptr)` during slot reuse to fully reinitialize the header.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Slot reuse detection via generation counter is working correctly, but `is_dropping` is a state flag that must also be reset. In Chez Scheme's GC, all object metadata is cleared on reuse to prevent stale state from affecting new allocations.

**Rustacean (Soundness 觀點):**
This is not a safety violation (no UB), but a memory leak. The `drop_fn` not being called means `Drop` implementations never run, which can cause resource leaks (file handles, etc.) rather than memory safety issues.

**Geohot (Exploit 觀點):**
While not directly exploitable for memory corruption, uncontrolled resource leaks could lead to denial-of-service in long-running GC applications. The generation check in `dec_ref` provides some protection but doesn't fix the root cause.

---

## Resolution (2026-03-28)

**Verified fixed in tree:** `GcBox::clear_is_dropping()` exists in `crates/rudo-gc/src/ptr.rs` and is invoked when a slot is taken from the free list in `LocalHeap::try_pop_from_page` (alongside `clear_dead`, `clear_gen_old`, `clear_under_construction`, `increment_generation`). The small-object dealloc path that returns a slot to the free list also calls `clear_is_dropping()` so stale dropping state is not left on reclaimed memory. No further code change was required for this issue.

**Check:** `cargo test -p rudo-gc test_free_slot_reuse --all-features -- --test-threads=1` (exercises collect + reuse); passed.
