# [Bug]: clone_orphan_root_with_inc_ref dereferences before is_allocated check

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Race window exists between ptr acquisition and dereference |
| **Severity (嚴重程度)** | `High` | UAF if slot swept without reuse; generation check does NOT catch sweep-only case |
| **Reproducibility (復現難度)** | `Medium` | Requires GC sweep to reclaim slot without immediate reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap.rs::clone_orphan_root_with_inc_ref`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `current`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
The `is_allocated` check should occur **before** dereferencing the GcBox pointer, as it does in the TCB clone path (`cross_thread.rs` lines 778-785):
```rust
// TCB path (correct order):
if let Some(idx) = ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    assert!((*header.as_ptr()).is_allocated(idx), ...);  // Check FIRST
}
let gc_box = &*self.ptr.as_ptr();  // THEN dereference
```

### 實際行為 (Actual Behavior)
In `clone_orphan_root_with_inc_ref()` (line 304), the code dereferences **before** checking `is_allocated`:
```rust
// Line 304 - BUG: dereferences before is_allocated check
let gc_box = &*ptr.as_ptr();  // UAF if slot was swept!

// Lines 305-310 - checks dead/dropping/under_construction flags
assert!(
    !gc_box.has_dead_flag()
        && gc_box.dropping_state() == 0
        && !gc_box.is_under_construction(),
    ...
);

// Lines 312-318 - is_allocated check comes TOO LATE
if let Some(idx) = ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = ptr_to_page_header(ptr.as_ptr() as *const u8);
    assert!((*header.as_ptr()).is_allocated(idx), ...);
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Ordering Violation:** The `is_allocated` check must precede pointer dereference to prevent UAF.

1. **Line 304**: `let gc_box = &*ptr.as_ptr();` - dereferences without checking if slot is allocated
2. **Lines 312-318**: `is_allocated` check happens AFTER dereference - TOCTOU window exists
3. **Lines 327-330**: Generation check catches slot **reuse** but NOT slot **sweep without reuse** (generation unchanged)

**Race Window:**
- Between `ptr_to_object_index()` succeeding and line 304 (dereference)
- Between line 304 (dereference) and lines 312-318 (is_allocated check)
- If GC sweep runs and reclaims the slot in either window, and slot is NOT immediately reused (generation unchanged), the generation check won't catch it

**Why generation check fails for sweep-only case:**
- Generation only increments on slot **reuse** (allocation with new generation)
- If slot is swept and left empty (NOT reused), generation remains unchanged
- The generation check at lines 327-330 passes even though memory is freed

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Pseudocode - requires precise timing with GC sweep
fn reproduce() {
    // 1. Create orphan root with known generation
    let gc = Gc::new(Data);
    let handle = gc.downgrade_handle();
    drop(gc);  // Drop origin, making it orphan
    
    // 2. Trigger slot sweep WITHOUT immediate reuse
    // (Requires memory pressure to force sweep but no immediate allocation)
    force_gc_sweep_without_reuse();
    
    // 3. Clone the orphan - BUG: dereferences freed memory
    if let Some(cloned) = handle.clone_orphan() {
        // May read stale/ wrong type data from freed slot
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. Move `is_allocated` check **before** line 304 dereference
2. The existing generation check (lines 322-330) remains as defense-in-depth for slot reuse
3. Ensure ordering: `is_allocated` -> `has_dead_flag/dropping_state/is_under_construction` -> `generation` -> `inc_ref`

```rust
unsafe {
    // FIX: Check is_allocated BEFORE dereferencing
    if let Some(idx) = ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = ptr_to_page_header(ptr.as_ptr() as *const u8);
        assert!(
            (*header.as_ptr()).is_allocated(idx),
            "clone_orphan_root_with_inc_ref: object slot was swept"
        );
    }
    
    let gc_box = &*ptr.as_ptr();
    assert!(
        !gc_box.has_dead_flag()
            && gc_box.dropping_state() == 0
            && !gc_box.is_under_construction(),
        "GcHandle::clone: cannot clone a dead, dropping, or under construction GcHandle (orphan)"
    );
    
    // ... rest of function unchanged
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The ordering violation in `clone_orphan_root_with_inc_ref` is a classic TOCTOU bug. In Chez Scheme's GC, we always verify slot validity before dereferencing. The generation check alone is insufficient for sweep-only cases because generation doesn't change when a slot is reclaimed without reuse. This creates a window where freed memory could be dereferenced if the timing aligns with lazy sweep.

**Rustacean (Soundness 觀點):**
This is undefined behavior - dereferencing a pointer to a deallocated object is UB regardless of whether the data is "still valid" in some sense. The `GcBox` points to heap memory that has been returned to the allocator. Even if the memory hasn't been reused yet, reading from it after free is UB. The `is_allocated` check must precede the dereference.

**Geohot (Exploit 觀點):**
If an attacker can control GC timing and force a sweep without reuse, they could:
1. Cause the orphan clone to read stale data from freed slot
2. If the freed slot is then reallocated to attacker-controlled data, type confusion becomes possible
3. The generation check provides some protection against reuse, but not against immediate dereference after sweep

---

## 🔗 相關程式碼 (Related Code)

- **正確範例 (TCB path):** `crates/rudo-gc/src/handles/cross_thread.rs` lines 778-785
- **錯誤範例 (Orphan path):** `crates/rudo-gc/src/heap.rs` lines 303-318