# [Bug]: AsyncHandleScope::handle missing generation check for TOCTOU slot reuse

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Race window is small but triggered during concurrent GC + handle creation |
| **Severity (嚴重程度)** | `Critical` | Type confusion from slot reuse could cause UAF or memory corruption |
| **Reproducibility (復現難度)** | `Medium` | Requires concurrent GC sweep during handle creation; stress testing may reveal |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandleScope::handle` (handles/async.rs)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

`AsyncHandleScope::handle()` is missing a generation check before dereferencing the GcBox pointer, creating a TOCTOU race condition. If the slot is swept and reused between the `is_allocated` check and the dereference, the code could read from a different (newly allocated) object, causing type confusion.

### 預期行為 (Expected Behavior)
`AsyncHandleScope::handle()` should have the same liveness checks as `GcScope::spawn()`, which includes:
1. `is_allocated` check
2. **Generation check BEFORE dereference** to detect slot reuse
3. Flag checks (dead, dropping, under_construction)

### 實際行為 (Actual Behavior)
`AsyncHandleScope::handle()` only performs:
1. `is_allocated` check
2. **NO generation check before dereference**
3. Flag checks after dereference

The comment at line 327 states "Matches GcScope::spawn() liveness checks" but this is incorrect - the generation check is missing.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Bug Location:** `crates/rudo-gc/src/handles/async.rs`, lines 326-344

**Problematic Code:**
```rust
// Liveness checks: ensure tracked object was not swept or reclaimed (bug248).
// Matches GcScope::spawn() liveness checks.  <-- INCORRECT COMMENT
let gc_box_ptr = gc_ptr as *const GcBox<()>;
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        assert!(
            (*header.as_ptr()).is_allocated(idx),
            "AsyncHandleScope::handle: object slot was swept"
        );
    }
    let gc_box = &*gc_box_ptr;  // <-- TOCTOU: no generation check before dereference!
    assert!(
        !gc_box.has_dead_flag()
            && gc_box.dropping_state() == 0
            && !gc_box.is_under_construction(),
        ...
    );
}
```

**Correct Pattern (from GcScope::spawn, lines 1299-1320):**
```rust
// Liveness checks: ensure tracked object was not swept or reclaimed (bug248).
let pre_generation: u32;
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(tracked.ptr as *const u8) {
        let header = crate::heap::ptr_to_page_header(tracked.ptr as *const u8);
        assert!(
            (*header.as_ptr()).is_allocated(idx),
            "GcScope::spawn: tracked object was deallocated"
        );
    }
    // Get generation BEFORE dereference to detect slot reuse (bugXXX).
    // If slot is swept and reused between is_allocated check and dereference,
    // generation will differ.
    pre_generation = (*tracked.ptr).generation();
}
let gc_box = unsafe { &*tracked.ptr };
// FIX bugxxx: Verify generation hasn't changed (slot was NOT reused).
if pre_generation != gc_box.generation() {
    panic!(
        "GcScope::spawn: slot was reused between liveness check and dereference"
    );
}
```

**Race Scenario:**
1. Thread A: Calls `AsyncHandleScope::handle(gc)` where `gc` points to slot S
2. Thread A: Passes `is_allocated(S)` check - returns true (slot S is allocated)
3. Thread B: GC sweep runs, reclaims slot S, allocates new object T with different generation
4. Thread A: Dereferences `gc_box_ptr`, reading from object T instead of original object
5. Thread A: Reads flags from object T (type confusion) - could pass/fail incorrectly

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// High-level concept - actual PoC requires Miri or stress testing
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;

#[derive(Trace)]
struct Data1 { value: i32 }

#[derive(Trace)]
struct Data2 { pointer: Gc<Data1> }  // Different type!

async fn trigger_bug() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = AsyncHandleScope::new(&tcb);
    
    // Create initial object
    let gc1 = Gc::new(Data1 { value: 42 });
    let handle1 = scope.handle(&gc1);
    
    // Stress test: Create many handles while GC runs concurrently
    // The race condition could cause a slot to be reused between
    // is_allocated check and dereference in handle()
    
    // If bug triggers: handle1 would read from wrong object type
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Add generation check to `AsyncHandleScope::handle()` to match `GcScope::spawn()`:

```rust
// Liveness checks: ensure tracked object was not swept or reclaimed (bug248).
// Matches GcScope::spawn() liveness checks.
let gc_box_ptr = gc_ptr as *const GcBox<()>;
unsafe {
    if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        assert!(
            (*header.as_ptr()).is_allocated(idx),
            "AsyncHandleScope::handle: object slot was swept"
        );
    }
    // FIX bug476: Get generation BEFORE dereference to detect slot reuse.
    // If slot is swept and reused between is_allocated check and dereference,
    // generation will differ.
    let pre_generation = (*gc_box_ptr).generation();
    
    let gc_box = &*gc_box_ptr;
    
    // FIX bug476: Verify generation hasn't changed (slot was NOT reused).
    if pre_generation != gc_box.generation() {
        panic!(
            "AsyncHandleScope::handle: slot was reused between liveness check and dereference"
        );
    }
    
    assert!(
        !gc_box.has_dead_flag()
            && gc_box.dropping_state() == 0
            && !gc_box.is_under_construction(),
        "AsyncHandleScope::handle: cannot track a dead, dropping, or under construction Gc"
    );
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The generation check is essential in concurrent GC systems. Without it, we have a TOCTOU window where the slot state can change between validation and use. The generation mechanism exists precisely to detect this - it should be used consistently everywhere we validate slot liveness. The comment "Matches GcScope::spawn() liveness checks" is misleading and should be corrected or the implementation should be made consistent.

**Rustacean (Soundness 觀點):**
This is a soundness issue. Reading from a dereferenced pointer that may have been reallocated to a different type violates Rust's type safety guarantees. The `is_allocated` check alone is insufficient because there's no guarantee the object at that slot hasn't changed between the check and the dereference. The generation check closes this window. This pattern should be enforced everywhere, possibly through a shared helper function to ensure consistency.

**Geohot (Exploit 觀點):**
Type confusion bugs are powerful exploit primitives. If an attacker can control the timing of GC sweep relative to handle creation, they could potentially make a handle point to a crafted object with specific memory layout. Even without full control, the race condition could lead to accessing dead objects or corrupted state. The small race window makes this difficult but not impossible - sophisticated attackers excel at timing attacks.

(End of file - total 178 lines)