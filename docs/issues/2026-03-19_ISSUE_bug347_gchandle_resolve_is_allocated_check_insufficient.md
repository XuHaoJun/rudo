# [Bug]: GcHandle::resolve_impl is_allocated check insufficient - does not prevent slot reuse TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires precise timing of lazy sweep slot reuse |
| **Severity (嚴重程度)** | Critical | Ref count corruption of wrong object leading to UAF or leak |
| **Reproducibility (復現難度)** | Very High | Requires concurrent lazy sweep and handle resolve |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve_impl`, `GcHandle::try_resolve_impl` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current (after bug345 fix)

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

The `is_allocated` check before `inc_ref()` should prevent TOCTOU where the slot could be swept and reused. It should verify that the **same object** the handle references is still in the slot.

### 實際行為 (Actual Behavior)

The `is_allocated(idx)` check only verifies that **some object** is allocated in the slot, not that it's the **same object** the handle references.

**Race scenario:**
1. Handle points to object A in slot X
2. Object A becomes unreachable, lazy sweep reclaims slot X
3. New object B is allocated in slot X (same address)
4. `is_allocated(idx)` returns `true` (slot is allocated with B)
5. `inc_ref()` is called on B's GcBox - **wrong object!**
6. B's ref_count is corrupted

---

## 🔬 根本原因分析 (Root Cause Analysis)

In `handles/cross_thread.rs` lines 223-234:

```rust
// Check is_allocated BEFORE inc_ref to avoid TOCTOU (bug345).
// The slot could be swept and reused between flag check and inc_ref,
// causing inc_ref to modify the wrong object's ref count.
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "GcHandle::resolve: object slot was swept before inc_ref"
    );
}

gc_box.inc_ref();  // BUG: operates on potentially NEW object B!
```

The comment claims to prevent bug345, but `is_allocated` only checks slot occupancy, not object identity.

**Why the post-increment check doesn't catch this:**
Lines 238-241 check `dropping_state() != 0 || has_dead_flag()`, but a newly allocated object B would have both as false/0, so the check passes.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Requires precise thread interleaving with lazy sweep:

```rust
// Conceptual PoC - requires TSan or extreme timing control
// Thread 1: resolve() on handle to A
// Thread 2: lazy sweep + allocate B in same slot
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Options to properly fix TOCTOU:

1. **Generation/version check**: Store a generation in GcBox header, verify it matches before inc_ref
2. **Hold lock during entire operation**: Hold the root table lock from check through inc_ref (already done for check, but slot could still be reused)
3. **CAS instead of inc_ref**: Use `compare_exchange` to atomically verify object identity and increment
4. **Store unique object ID in handle**: Compare current object ID with handle's stored ID

The simplest fix is to store a `slot_generation` in each GcBox that increments on each allocation, and verify the generation matches before inc_ref.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This undermines the BiBOP invariant that a handle uniquely identifies an object. In a proper GC, slot reuse must not be observable through existing handles. The `is_allocated` check attempts this but fails because it only verifies slot state, not object identity.

**Rustacean (Soundness 觀點):**
This is memory corruption - `inc_ref` operating on the wrong object corrupts ref counts leading to use-after-free or memory leaks. The post-increment safety check is ineffective because a newly allocated object has valid (0, false) state.

**Geohot (Exploit 觀點):**
Exploit path: (1) Create handle to A, (2) A becomes unreachable, (3) Lazy sweep reclaims slot, (4) B allocated in same slot, (5) inc_ref corrupts B's ref_count, (6) If B is security-sensitive, manipulation via corrupted ref_count leads to UAF.

---

## Related Issues

- bug345: Original issue that added `is_allocated` check (but fix was insufficient)
- bug83: GcHandle resolve/clone TOCTOU race (different issue - handle unregistered)

---

## Fix Applied (2026-03-20)

**Fix:** Added per-object generation tracking to GcBox to detect slot reuse.

**Changes:**
1. Added `generation: AtomicU32` field to `GcBox` struct (ptr.rs)
2. Added `generation()` and `increment_generation()` methods to `GcBox`
3. Initialized `generation: 1` in all GcBox allocation paths
4. Increment generation on slot reuse in `try_pop_from_page` (heap.rs)
5. Added generation check before/after `inc_ref()` in `resolve_impl` (cross_thread.rs:234-242)
6. Added generation check before/after `inc_ref()` in `try_resolve_impl` (cross_thread.rs:354-361)
7. Updated test_large_struct_interior_pointer to account for new header size (48 bytes)

**Behavior:**
- When slot is swept and reused, generation increments
- During resolve, if generation changed between pre-check and inc_ref, panic (resolve_impl) or return None (try_resolve_impl)
- This prevents inc_ref from operating on wrong object's ref count

**Tests:** All cross_thread_handle tests pass (27 tests)

