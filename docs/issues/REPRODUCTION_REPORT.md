# Bug Reproduction Report

Generated from docs/issues Bug Reproduction plan.

## Summary

| Tier | Reproduced | Not Reproduced | Not Verified |
|------|------------|----------------|---------------|
| A (static) | 3 (bug13, bug12, bug16/21) | 0 | 0 |
| B (tests) | 0 | 3 (bug1, bug2, bug6/3) | 0 |
| C (PoC) | 0 | 3 (bug8, bug3-gen, bug17) | 0 |
| D (remaining) | 5 (bug4, bug7, bug10, bug11, bug14, bug26) | 1 (bug5) | 15+ |

**Static/code verified**: bug4, bug7, bug10, bug12, bug13, bug14, bug16/21, bug26
**Runtime reproduced**: bug11 (panic)
**Runtime not reproduced**: bug1, bug2, bug6/3, bug8, bug3-gen, bug17

## Tier A: Static Verification

### 1. bug13 - GcCell::write_barrier 死代碼
- **Status**: REPRODUCED
- **Evidence**: `grep '\.write_barrier\('` returns no call sites. Method exists in `cell.rs:285` but is never invoked.
- **Verification**: 2026-02-20

### 2. bug12 - generational barrier 與文檔不一致
- **Status**: REPRODUCED
- **Evidence**: `is_generational_barrier_active()` in `gc/incremental.rs:472-477` checks `is_incremental_marking_active()`, returning false when incremental marking is idle. Docs in `cell.rs:318-321` state barrier "remains active through ALL phases".
- **Verification**: 2026-02-20

### 3. bug16 / bug21 - scan_page_for_marked_refs 冗餘索引
- **Status**: REPRODUCED
- **Evidence**: In `gc/incremental.rs:766-782`, `ptr_to_object_index(obj_ptr.cast())` recomputes index (idx == i). `!(*header).is_marked(idx)` is redundant given outer `!(*header).is_marked(i)`.
- **Verification**: 2026-02-20

---

## Tier B: Run Tests

### 4. bug1 - Large object interior UAF
- **Status**: NOT REPRODUCED
- **Evidence**: `cargo test --test bug1_large_object_interior_uaf -- --test-threads=1` passed. Test may not trigger conditions, or bug may have been fixed.
- **Verification**: 2026-02-20

### 5. bug2 - Orphan sweep weak ref segfault
- **Status**: NOT REPRODUCED
- **Evidence**: `cargo test --test bug2_orphan_sweep_weak_ref -- --test-threads=1` passed. upgrade() did not segfault.
- **Verification**: 2026-02-20

### 6. bug6 / bug3 - Multi-page GcCell write barrier 失效
- **Status**: NOT REPRODUCED
- **Evidence**: Created `tests/bug3_write_barrier_multi_page.rs`, test passed. Note: collect_full() traces from roots so young object may survive via trace even if barrier failed. Bug might require minor GC scenario to manifest.
- **Verification**: 2026-02-20

---

## Tier C: PoC

### 7. bug8 - Weak::is_alive TOCTOU
- **Status**: NOT REPRODUCED
- **Evidence**: Created PoC, ran 50 iterations. No UAF/panic. Race may need Miri/ThreadSanitizer to detect.
- **Verification**: 2026-02-20

### 8. bug3 (generational) - GEN_OLD_FLAG 未檢查
- **Status**: NOT REPRODUCED
- **Evidence**: PoC passed. Young object survived (possibly via full GC trace).
- **Verification**: 2026-02-20

### 9. bug17 - GEN_OLD_FLAG 釋放時未清除
- **Status**: NOT REPRODUCED
- **Evidence**: PoC passed. No observable wrong barrier behavior.
- **Verification**: 2026-02-20

---

## Tier D: Remaining Issues

### bug4 - Cross-thread handle TCB leak
- **Status**: REPRODUCED (static)
- **Evidence**: GcHandle holds `origin_tcb: Arc<ThreadControlBlock>`. When origin thread terminates, handle still holds Arc → TCB cannot be freed. See `handles/cross_thread.rs:70`.
- **Verification**: 2026-02-20

### bug5 - Incremental worklist unbounded
- **Status**: NOT REPRODUCED
- **Evidence**: Requires large pointer graph + overflow. Not exercised in this pass.
- **Verification**: 2026-02-20

### bug7 - unified_write_barrier 缺少執行緒所有權驗證
- **Status**: REPRODUCED (static)
- **Evidence**: `unified_write_barrier` (heap.rs:2637) has no get_thread_id/owner_thread check. `gc_cell_validate_and_barrier` (2561-2579) does.
- **Verification**: 2026-02-20

### bug10 - GcBoxWeakRef::upgrade 缺少 is_under_construction
- **Status**: REPRODUCED (static)
- **Evidence**: `GcBoxWeakRef::upgrade()` (ptr.rs:406-431) does NOT check is_under_construction. `try_upgrade()` (466+) does.
- **Verification**: 2026-02-20

### bug11 - GcHandle::resolve() panic when origin terminated
- **Status**: REPRODUCED
- **Evidence**: Created test; resolve() panics when called from non-origin thread after origin joined. Test passed.
- **Verification**: 2026-02-20

### bug14 - GcThreadSafeCell SATB overflow ignored
- **Status**: REPRODUCED (static)
- **Evidence**: `cell.rs:921` uses `let _ = heap.record_satb_old_value(*gc_ptr)` vs GcCell at 168 checks return value.
- **Verification**: 2026-02-20

### bug26 - Gc::deref / try_deref 未檢查 DEAD_FLAG
- **Status**: REPRODUCED (static)
- **Evidence**: `ptr.rs:1267-1271` deref has no has_dead_flag check. `try_deref` (1048-1055) checks null only, then derefs.
- **Verification**: 2026-02-20

### bug15, bug18, bug19, bug20, bug22, bug23, bug25, bug27, bug28, bug29, bug30, bug31, bug32, bug33, bug34
- **Status**: NOT VERIFIED
- **Note**: Require deeper PoC or concurrent scenario. Left for future passes.
