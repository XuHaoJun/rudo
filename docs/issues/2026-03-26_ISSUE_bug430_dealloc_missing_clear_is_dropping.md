# [Bug]: dealloc missing clear_is_dropping causes dropping state leak on slot reuse

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | Any explicit dealloc that returns slot to free list |
| **Severity (嚴重程度)** | High | Memory leak - dropped values never have their Drop run |
| **Reproducibility (重現難度)** | Medium | Requires specific object lifecycle with explicit dealloc |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `dealloc` in `heap.rs`, `dec_ref` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

When a slot is explicitly deallocated via `dealloc` in `heap.rs`, the `is_dropping` field is NOT cleared, unlike other flags (`DEAD_FLAG`, `GEN_OLD_FLAG`, `UNDER_CONSTRUCTION_FLAG`).

### 預期行為 (Expected Behavior)
When a slot is deallocated and returned to the free list, all GC-related flags should be reset so the slot starts with a clean state when eventually reused.

### 實際行為 (Actual Behavior)
If a slot is deallocated with `is_dropping = 1` or `is_dropping = 2`, and that slot is later reused, the `dropping_state` persists from the previous object.

When `dec_ref` is called on the new object with `ref_count == 1`:
```rust
// ptr.rs:189
if count == 1 && this.dropping_state() == 0 {
    if this.try_mark_dropping() {
        unsafe { (this.drop_fn)(self_ptr.cast::<u8>()); }
    }
}
```

Since `dropping_state != 0`, the code falls through and just decrements `ref_count` to 0 WITHOUT calling `drop_fn`. The new object's value is NEVER dropped - a memory leak!

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `heap.rs` at `dealloc` (lines 2754-2760):

```rust
// Clear DEAD_FLAG, GEN_OLD_FLAG, and UNDER_CONSTRUCTION_FLAG so reused slots
// don't inherit stale state.
unsafe {
    (*gc_box_ptr).clear_dead();
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();
    // MISSING: (*gc_box_ptr).clear_is_dropping();
}
```

Compare with `try_pop_from_page` in `heap.rs` (lines 2323-2330) which correctly clears all flags:
```rust
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_dead();
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();
    (*gc_box_ptr).clear_is_dropping();  // <-- This IS called here
    (*gc_box_ptr).increment_generation();
    (*header).clear_dirty(idx as usize);
}
```

The `dealloc` function does NOT call `clear_is_dropping()`, but `try_pop_from_page` does. This inconsistency means explicit deallocation can leave stale `is_dropping` state that affects newly allocated objects in the same slot.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This would need to be run with specific conditions:
// 1. Create object A
// 2. Let object A's ref_count go to 1 and enter dropping phase (is_dropping = 1)
// 3. Explicitly deallocate A via Gc::dealloc or page dealloc
// 4. Allocate new object B in the same slot
// 5. Drop B - its value will NOT be dropped because dropping_state inherited from A
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `(*gc_box_ptr).clear_is_dropping();` in `dealloc` at line 2760, alongside the other flag clearing operations:

```rust
unsafe {
    (*gc_box_ptr).clear_dead();
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();
    (*gc_box_ptr).clear_is_dropping();  // <-- Add this line
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Slot reuse via explicit dealloc should sanitize all flags, not just some. The `is_dropping` state is critical for proper drop implementation - if it persists across slot reuse, the new object's `Drop` never runs.

**Rustacean (Soundness 觀點):**
This is a memory leak, not a safety violation. The `drop_fn` not being called means `Drop` implementations never run, causing resource leaks.

**Geohot (Exploit 觀點):**
Resource leaks can lead to denial-of-service in long-running GC applications.

---

## Resolution (2026-03-28)

**Verified in code:** `LocalHeap::dealloc` in `crates/rudo-gc/src/heap.rs` already clears `is_dropping` together with the other slot flags before returning the slot to the free list (`clear_dead`, `clear_gen_old`, `clear_under_construction`, `clear_is_dropping`). No additional code change was required. `cargo test -p rudo-gc --lib --tests --all-features -- --test-threads=1` passed.