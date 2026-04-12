# [Bug]: alloc_slow does not initialize all GcBox fields

**Status:** Open
**Tags:** Unverified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | Fresh slots use TLAB allocation; slot reuse path is rarely hit on first allocation |
| **Severity (嚴重程度)** | Medium | Uninitialized memory read (UB) and potential flag corruption |
| **Reproducibility (復現難度)** | High | Miri would detect the UB; flag corruption is hard to observe |

---

## 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `alloc_slow` in `heap.rs`, `try_pop_from_page` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 問題描述 (Description)

### 預期行為 (Expected Behavior)
All `GcBox` fields should be properly initialized before a slot is used for allocation.

### 實際行為 (Actual Behavior)
In `alloc_slow` (heap.rs:2483-2492), only `drop_fn` and `trace_fn` are initialized:
```rust
for i in 0..obj_count {
    let obj_ptr = ptr.as_ptr().add(h_size + (i * block_size));
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    std::ptr::addr_of_mut!((*gc_box_ptr).drop_fn)
        .write(crate::ptr::GcBox::<()>::no_op_drop);
    std::ptr::addr_of_mut!((*gc_box_ptr).trace_fn)
        .write(crate::ptr::GcBox::<()>::no_op_trace);
}
```

The fields `ref_count`, `weak_count`, `is_dropping`, and `generation` are NOT initialized.

When a fresh slot is allocated via TLAB (first use), the slot has uninitialized fields. When that slot is freed and later reallocated via `try_pop_from_page`, `clear_gen_old()` reads `weak_count` which contains uninitialized memory.

---

## 根本原因分析 (Root Cause Analysis)

`GcBox` layout (64-bit):
```rust
pub struct GcBox<T: Trace + ?Sized> {
    ref_count: AtomicUsize,          // offset 0 (8 bytes)
    weak_count: AtomicUsize,          // offset 8 (8 bytes)
    drop_fn: unsafe fn(*mut u8),      // offset 16 (8 bytes) - INITIALIZED
    trace_fn: unsafe fn(*const u8, &mut GcVisitor), // offset 24 (8 bytes) - INITIALIZED
    is_dropping: AtomicUsize,         // offset 32 (8 bytes) - UNINITIALIZED
    generation: AtomicU32,            // offset 40 (4 bytes) - UNINITIALIZED
    value: T,                        // offset 44+
}
```

In `alloc_slow`, only `drop_fn` (offset 16) and `trace_fn` (offset 24) are initialized.

When a slot is freed in `sweep_page`:
- `obj_cast.write_unaligned(current_free)` writes `Option<u16>` to offset 0 (where `ref_count` lives)
- `weak_count` at offset 8 is NOT written

When reallocating via `try_pop_from_page`:
```rust
unsafe {
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    (*gc_box_ptr).clear_gen_old();  // Reads uninitialized weak_count at offset 8!
    (*gc_box_ptr).clear_under_construction();
    (*gc_box_ptr).clear_is_dropping();
    (*gc_box_ptr).increment_generation();
}
```

`clear_gen_old()` performs `self.weak_count.fetch_and(!GEN_OLD_FLAG)`, which reads uninitialized memory.

---

## 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Miri would detect this UB. A direct Rust test cannot reliably trigger this because:
1. Fresh slots use TLAB bump allocation (not free list)
2. Slot must be freed and reallocated to hit the bug

---

## 建議修復方案 (Suggested Fix / Remediation)

In `alloc_slow`, initialize all GcBox fields, not just `drop_fn` and `trace_fn`:

```rust
for i in 0..obj_count {
    let obj_ptr = ptr.as_ptr().add(h_size + (i * block_size));
    let gc_box_ptr = obj_ptr.cast::<crate::ptr::GcBox<()>>();
    // Initialize ref_count and weak_count to 0
    std::ptr::addr_of_mut!((*gc_box_ptr).ref_count)
        .write(AtomicUsize::new(0));
    std::ptr::addr_of_mut!((*gc_box_ptr).weak_count)
        .write(AtomicUsize::new(0));
    std::ptr::addr_of_mut!((*gc_box_ptr).drop_fn)
        .write(crate::ptr::GcBox::<()>::no_op_drop);
    std::ptr::addr_of_mut!((*gc_box_ptr).trace_fn)
        .write(crate::ptr::GcBox::<()>::no_op_trace);
    std::ptr::addr_of_mut!((*gc_box_ptr).is_dropping)
        .write(AtomicUsize::new(0));
    std::ptr::addr_of_mut!((*gc_box_ptr).generation)
        .write(AtomicU32::new(0));
}
```

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The uninitialized `weak_count` could corrupt flag state (DEAD_FLAG, UNDER_CONSTRUCTION_FLAG, GEN_OLD_FLAG) when slots are reused. This could lead to incorrect GC decisions about object liveness. However, the bug only manifests on slot reuse after the first allocation cycle, which may be rare in practice.

**Rustacean (Soundness 觀點):**
This is clearly undefined behavior: reading uninitialized memory in `clear_gen_old()`. Miri would detect this. The `fetch_and` operation reads `weak_count` before writing, which is UB even if the write would set a valid value.

**Geohot (Exploit 觀點):**
Uninitialized memory could potentially contain sensitive data from previous allocations. However, since this is a GC heap, the memory is already from the process heap. The more concerning aspect is that corrupted flag state could lead to use-after-free if DEAD_FLAG is incorrectly cleared.