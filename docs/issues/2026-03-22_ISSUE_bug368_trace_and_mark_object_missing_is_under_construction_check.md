# [Bug]: trace_and_mark_object missing is_under_construction check

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires allocation-triggered safepoint during Gc::new_cyclic_weak |
| **Severity (嚴重程度)** | High | Could trace partially initialized object, causing UAF |
| **Reproducibility (重現難度)** | Medium | Requires precise timing of safepoint during allocation |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Incremental Marking`, `trace_and_mark_object` in `gc/incremental.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
All functions that trace or mark GC objects should check `is_under_construction()` to skip partially initialized objects (e.g., during `Gc::new_cyclic_weak`). This prevents tracing uninitialized fields which could cause undefined behavior.

### 實際行為 (Actual Behavior)
`trace_and_mark_object` (incremental.rs:785-812) calls `trace_fn` at line 807 WITHOUT checking `is_under_construction()`. This is inconsistent with all other similar functions in the same file:

- `scan_page_for_marked_refs` (line 862): HAS `is_under_construction()` check
- `scan_page_for_unmarked_refs` (line 1016): HAS `is_under_construction()` check  
- `mark_new_object_black` (line 1063): HAS `is_under_construction()` check
- `mark_object_black` (line 1119): HAS `is_under_construction()` check

---

## 🔬 根本原因分析 (Root Cause Analysis)

**文件:** `crates/rudo-gc/src/gc/incremental.rs:785-812`

Buggy code (line 807):
```rust
unsafe fn trace_and_mark_object(gc_box: NonNull<GcBox<()>>, state: &IncrementalMarkState) {
    // ... validation code (lines 786-799) ...
    
    // BUG: No is_under_construction check before trace_fn call!
    let mut visitor = crate::trace::GcVisitor::new(crate::trace::VisitorKind::Major);
    ((*gc_box.as_ptr()).trace_fn)(data_ptr, &mut visitor);  // Line 807 - MISSING CHECK!
    // ...
}
```

**問題:** `trace_and_mark_object` is called during incremental marking from `mark_slice`. If a thread enters safepoint during `Gc::new_cyclic_weak` allocation, the partially initialized object (with `is_under_construction = true`) could be in the worklist and processed by this function.

**對比其他函數:**
```rust
// scan_page_for_unmarked_refs (line 1016) - CORRECT
if unsafe { (*gc_box_ptr).is_under_construction() } {
    (*header).clear_mark_atomic(i);
    continue;
}

// mark_new_object_black (line 1063) - CORRECT  
if gc_box.is_under_construction() {
    return false;
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Requires precise timing - allocation-triggered safepoint during Gc::new_cyclic_weak
// 1. Thread A: Call Gc::new_cyclic_weak
// 2. Thread A: GcBox allocated, UNDER_CONSTRUCTION_FLAG set
// 3. Thread A: check_safepoint() triggered (e.g., by TLAB exhaustion)
// 4. Thread A: enter_rendezvous(), thread paused at safepoint
// 5. GC thread: execute_snapshot runs, marks object and adds to worklist
// 6. GC thread: mark_slice calls trace_and_mark_object on partially initialized object
// 7. GC thread: trace_fn is called on uninitialized fields - POTENTIAL UAF!
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add `is_under_construction` check before calling `trace_fn`:

```rust
unsafe fn trace_and_mark_object(gc_box: NonNull<GcBox<()>>, state: &IncrementalMarkState) {
    let ptr = gc_box.as_ptr() as *const u8;
    let header = crate::heap::ptr_to_page_header(ptr);

    // ... existing validation code ...

    // FIX: Skip partially initialized objects (e.g. during Gc::new_cyclic_weak).
    // Avoids tracing uninitialized fields which could cause UAF.
    if (*gc_box.as_ptr()).is_under_construction() {
        return;
    }

    let block_size = (*header.as_ptr()).block_size as usize;
    let header_size = crate::heap::PageHeader::header_size(block_size);
    let data_ptr = ptr.add(header_size);

    let mut visitor = crate::trace::GcVisitor::new(crate::trace::VisitorKind::Major);
    ((*gc_box.as_ptr()).trace_fn)(data_ptr, &mut visitor);
    // ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The consistency issue is clear - all other marking/trace functions in incremental.rs have this check. During `Gc::new_cyclic_weak`, the object is partially initialized when `is_under_construction` is true. If the allocating thread enters GC rendezvous during allocation (via `check_safepoint`), the worklist could contain this partially initialized object. Tracing it would access uninitialized fields.

**Rustacean (Soundness 觀點):**
Tracing uninitialized memory is undefined behavior in Rust. Even if the fields happen to be zero-initialized, accessing them through a partially constructed object violates Rust's safety guarantees. The `is_under_construction` flag exists precisely to prevent this.

**Geohot (Exploit 觀點):**
If an attacker can trigger a safepoint during a victim's `Gc::new_cyclic_weak` allocation, they could cause the GC to trace uninitialized memory. This could potentially be exploited to leak sensitive data from uninitialized heap memory, or cause a crash if the uninitialized data contains invalid pointers.
