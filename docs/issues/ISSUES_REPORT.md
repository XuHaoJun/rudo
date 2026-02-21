# Bug Issues Report

## Statistics

### By Status
- **Fixed**: 32
- **Open**: 24
- **Invalid**: 3

### By Tags
- **Verified**: 28
- **Not Verified**: 25
- **Not Reproduced**: 6

## All Issues

| Issue | Title | Status | Tags |
|---|---|---|---|
| [2026-02-19_ISSUE_bug1_large_object_interior_uaf.md](./2026-02-19_ISSUE_bug1_large_object_interior_uaf.md) | 大型物件內部指標在執行緒終止後失效導致 UAF | Fixed | Not Reproduced |
| [2026-02-19_ISSUE_bug2_orphan_sweep_weak_ref.md](./2026-02-19_ISSUE_bug2_orphan_sweep_weak_ref.md) | 孤立物件的 Weak 參考在回收時導致記憶體錯誤 | Fixed | Not Reproduced |
| [2026-02-19_ISSUE_bug3_generational_barrier_gen_old_flag.md](./2026-02-19_ISSUE_bug3_generational_barrier_gen_old_flag.md) | Generational Write Barrier 忽略 per-object GEN_OLD_FLAG 導致 OLD→YOUNG 引用遺漏 | Fixed | Not Reproduced |
| [2026-02-19_ISSUE_bug4_cross_thread_handle_tcb_leak.md](./2026-02-19_ISSUE_bug4_cross_thread_handle_tcb_leak.md) | Origin 執行緒終止後 GcHandle 持有無效的 Arc<ThreadControlBlock> 導致記憶體洩露 | Fixed | Verified |
| [2026-02-19_ISSUE_bug5_incremental_worklist_unbounded.md](./2026-02-19_ISSUE_bug5_incremental_worklist_unbounded.md) | Incremental Marking 增量標記階段 Overflow 時的 Worklist 無界成長 | Invalid | Not Reproduced |
| [2026-02-19_ISSUE_bug6_multi_page_gccell_barrier.md](./2026-02-19_ISSUE_bug6_multi_page_gccell_barrier.md) | Multi-Page Large Object 的 GcCell Write Barrier 在 Tail Pages 上失效 | Fixed | Verified |
| [2026-02-19_ISSUE_bug7_unified_barrier_thread_check.md](./2026-02-19_ISSUE_bug7_unified_barrier_thread_check.md) | unified_write_barrier 缺少執行緒所有權驗證 | Invalid | Verified |
| [2026-02-19_ISSUE_bug8_weak_is_alive_toctou.md](./2026-02-19_ISSUE_bug8_weak_is_alive_toctou.md) | Weak::is_alive() 存在 TOCTOU 競爭條件可能導致 Use-After-Free | Fixed | Verified |
| [2026-02-19_ISSUE_bug9_weak_is_alive_refcount.md](./2026-02-19_ISSUE_bug9_weak_is_alive_refcount.md) | Weak::is_alive() 不檢查 ref_count 導致不一致行為 | Fixed | Verified |
| [2026-02-19_ISSUE_bug10_gcbox_weak_ref_upgrade_construction.md](./2026-02-19_ISSUE_bug10_gcbox_weak_ref_upgrade_construction.md) | GcBoxWeakRef::upgrade() 缺少 is_under_construction 檢查 | Fixed | Verified |
| [2026-02-19_ISSUE_bug11_gchandle_origin_thread_terminated.md](./2026-02-19_ISSUE_bug11_gchandle_origin_thread_terminated.md) | GcHandle::resolve() 在原始執行緒終止後 panic | Fixed | Verified |
| [2026-02-19_ISSUE_bug12_generational_barrier_docs_inconsistent.md](./2026-02-19_ISSUE_bug12_generational_barrier_docs_inconsistent.md) | is_generational_barrier_active() 與文檔不一致 | Fixed | Verified |
| [2026-02-19_ISSUE_bug13_dead_code_write_barrier.md](./2026-02-19_ISSUE_bug13_dead_code_write_barrier.md) | GcCell::write_barrier() 是永遠不會被調用的死代碼 | Fixed | Verified |
| [2026-02-19_ISSUE_bug14_gcthreadsafecell_satb_overflow_ignored.md](./2026-02-19_ISSUE_bug14_gcthreadsafecell_satb_overflow_ignored.md) | GcThreadSafeCell::borrow_mut() 忽略 record_satb_old_value 返回值導致 SATB 不變性破壞 | Fixed | Verified |
| [2026-02-19_ISSUE_bug15_gcthreadsaferefmut_drop_uaf.md](./2026-02-19_ISSUE_bug15_gcthreadsaferefmut_drop_uaf.md) | GcThreadSafeRefMut::drop() 可能於並髮標記期間導致 UAF | Fixed | Verified |
| [2026-02-19_ISSUE_bug16_scan_page_redundant_index.md](./2026-02-19_ISSUE_bug16_scan_page_redundant_index.md) | scan_page_for_marked_refs 冗餘的物件索引計算 | Fixed | Verified |
| [2026-02-19_ISSUE_bug17_gen_old_flag_not_cleared.md](./2026-02-19_ISSUE_bug17_gen_old_flag_not_cleared.md) | GEN_OLD_FLAG 在物件釋放時未被清除，導致重新配置後產生錯誤的 barrier 行為 | Fixed | Not Reproduced |
| [2026-02-19_ISSUE_bug18_gcrwlock_gcmutex_drop_missing_satb.md](./2026-02-19_ISSUE_bug18_gcrwlock_gcmutex_drop_missing_satb.md) | GcRwLockWriteGuard 與 GcMutexGuard 缺少 Drop 時的 SATB Barrier，導致修改後的 GC 指針可能未被標記 | Fixed | Verified |
| [2026-02-19_ISSUE_bug19_gcscope_spawn_bounds_check.md](./2026-02-19_ISSUE_bug19_gcscope_spawn_bounds_check.md) | GcScope::spawn Missing Bounds Check Causes Buffer Overflow | Fixed | Verified |
| [2026-02-19_ISSUE_bug20_cross_thread_satb_buffer_unbounded.md](./2026-02-19_ISSUE_bug20_cross_thread_satb_buffer_unbounded.md) | Cross-Thread SATB Buffer Unbounded Growth Potential | Fixed | Not Reproduced |
| [2026-02-19_ISSUE_bug21_scan_page_redundant_index_check.md](./2026-02-19_ISSUE_bug21_scan_page_redundant_index_check.md) | Redundant Index Check in scan_page_for_marked_refs | Fixed | Verified |
| [2026-02-19_ISSUE_bug22_hashmap_gccapture_iterator_invalidation.md](./2026-02-19_ISSUE_bug22_hashmap_gccapture_iterator_invalidation.md) | HashMap GcCapture Potential Iterator Invalidation | Invalid | Not Verified |
| [2026-02-19_ISSUE_bug23_gcthreadsafecell_gccapture_data_race.md](./2026-02-19_ISSUE_bug23_gcthreadsafecell_gccapture_data_race.md) | GcThreadSafeCell GcCapture Implementation Data Race | Fixed | Verified |
| [2026-02-19_ISSUE_bug25_write_barrier_gen_old_relaxed_ordering.md](./2026-02-19_ISSUE_bug25_write_barrier_gen_old_relaxed_ordering.md) | Write Barrier 中 GEN_OLD_FLAG 讀取使用 Relaxed Ordering 導致潛在 Race Condition | Fixed | Verified |
| [2026-02-19_ISSUE_bug26_gc_deref_dead_flag.md](./2026-02-19_ISSUE_bug26_gc_deref_dead_flag.md) | Gc::deref 與 try_deref 未檢查 DEAD_FLAG 導致 Use-After-Free | Fixed | Verified |
| [2026-02-19_ISSUE_bug27_weak_upgrade_toctou.md](./2026-02-19_ISSUE_bug27_weak_upgrade_toctou.md) | Weak::upgrade() ref_count Relaxed 載入導致 TOCTOU Use-After-Free | Fixed | Verified |
| [2026-02-19_ISSUE_bug28_gcrwlock_capture_gc_ptrs_empty_slice.md](./2026-02-19_ISSUE_bug28_gcrwlock_capture_gc_ptrs_empty_slice.md) | GcRwLock::capture_gc_ptrs() 返回空切片導致 GC 遺漏內部指標 | Fixed | Verified |
| [2026-02-20_ISSUE_bug29_gchandle_clone_unregister_race.md](./2026-02-20_ISSUE_bug29_gchandle_clone_unregister_race.md) | GcHandle clone()/unregister() Race 導致物件在 Root 移除後仍被視為 Root | Fixed | Verified |
| [2026-02-20_ISSUE_bug30_gc_requested_relaxed_ordering.md](./2026-02-20_ISSUE_bug30_gc_requested_relaxed_ordering.md) | GC_REQUESTED Relaxed Ordering Causes Missed GC Handshake | Fixed | Verified |
| [2026-02-20_ISSUE_bug31_weak_clone_toctou.md](./2026-02-20_ISSUE_bug31_weak_clone_toctou.md) | Weak::clone has TOCTOU race causing potential use-after-free | Fixed | Verified |
| [2026-02-20_ISSUE_bug32_gcmutex_try_lock_missing_barrier.md](./2026-02-20_ISSUE_bug32_gcmutex_try_lock_missing_barrier.md) | GcMutex::try_lock() 缺少 Write Barrier 導致 SATB 不變性破壞 | Fixed | Verified |
| [2026-02-20_ISSUE_bug33_gcmutex_missing_gccapture.md](./2026-02-20_ISSUE_bug33_gcmutex_missing_gccapture.md) | GcMutex 缺少 GcCapture 實作導致 SATB 屏障失效 | Fixed | Verified |
| [2026-02-20_ISSUE_bug33_try_inc_ref_from_zero_resurrection.md](./2026-02-20_ISSUE_bug33_try_inc_ref_from_zero_resurrection.md) | try_inc_ref_from_zero 允許在有 weak references 時復活已死亡物件 | Fixed | Verified |
| [2026-02-20_ISSUE_bug34_gcrwlock_capture_try_read.md](./2026-02-20_ISSUE_bug34_gcrwlock_capture_try_read.md) | GcRwLock::capture_gc_ptrs_into 使用 try_read() 可能導致指標遺漏 | Fixed | Verified |
| [2026-02-20_ISSUE_bug35_std_rwlock_capture_try_read.md](./2026-02-20_ISSUE_bug35_std_rwlock_capture_try_read.md) | std::sync::RwLock 的 GcCapture 實作使用 try_read() 可能導致指標遺漏 | Open | Not Verified |
| [2026-02-20_ISSUE_bug36_std_mutex_missing_gccapture.md](./2026-02-20_ISSUE_bug36_std_mutex_missing_gccapture.md) | std::sync::Mutex 缺少 GcCapture 實作導致指標遺漏 | Open | Not Verified |
| [2026-02-20_ISSUE_bug37_arc_missing_gccapture.md](./2026-02-20_ISSUE_bug37_arc_missing_gccapture.md) | std::sync::Arc 缺少 GcCapture 實作導致指標遺漏 | Open | Not Verified |
| [2026-02-20_ISSUE_bug38_rc_missing_gccapture.md](./2026-02-20_ISSUE_bug38_rc_missing_gccapture.md) | std::rc::Rc 缺少 GcCapture 實作導致 SATB 屏障失效 | Open | Not Verified |
| [2026-02-20_ISSUE_bug39_gchandle_resolve_missing_validity_check.md](./2026-02-20_ISSUE_bug39_gchandle_resolve_missing_validity_check.md) | GcHandle::resolve() 缺少物件有效性驗證 | Open | Not Verified |
| [2026-02-20_ISSUE_bug40_zst_singleton_ref_count.md](./2026-02-20_ISSUE_bug40_zst_singleton_ref_count.md) | ZST Singleton 初始化時 ref_count 為 2 而非 1 | Open | Not Verified |
| [2026-02-20_ISSUE_bug41_gcbox_weak_upgrade_dropping_state.md](./2026-02-20_ISSUE_bug41_gcbox_weak_upgrade_dropping_state.md) | GcBoxWeakRef::upgrade() 未檢查 dropping_state 導致 Use-After-Free 風險 | Open | Not Verified |
| [2026-02-20_ISSUE_bug42_weak_try_upgrade_missing_dropping_state.md](./2026-02-20_ISSUE_bug42_weak_try_upgrade_missing_dropping_state.md) | Weak::try_upgrade() 缺少 dropping_state 檢查導致 Use-After-Free 風險 | Open | Not Verified |
| [2026-02-20_ISSUE_bug43_weak_ephemeron_missing_gccapture.md](./2026-02-20_ISSUE_bug43_weak_ephemeron_missing_gccapture.md) | Weak<T> and Ephemeron<K,V> missing GcCapture implementation | Open | Not Verified |
| [2026-02-21_ISSUE_bug44_gc_clone_missing_flag_check.md](./2026-02-21_ISSUE_bug44_gc_clone_missing_flag_check.md) | Gc::clone() 缺少 has_dead_flag 和 dropping_state 檢查導致異常行為 | Open | Not Verified |
| [2026-02-21_ISSUE_bug45_dirty_pages_snapshot_race.md](./2026-02-21_ISSUE_bug45_dirty_pages_snapshot_race.md) | Dirty Pages Snapshot Race 導致 Young 物件被錯誤回收 | Open | Not Verified |
| [2026-02-21_ISSUE_bug46_gc_clone_missing_dead_flag_check.md](./2026-02-21_ISSUE_bug46_gc_clone_missing_dead_flag_check.md) | Gc::clone() Missing Dead Flag Check 導致記憶體不安全 | Open | Not Verified |
| [2026-02-21_ISSUE_bug47_gc_as_ptr_doc_mismatch.md](./2026-02-21_ISSUE_bug47_gc_as_ptr_doc_mismatch.md) | Gc::as_ptr() 文件與實作不符 - 文件說會 panic 但實際不會 | Fixed | Verified |
| [2026-02-21_ISSUE_bug48_gc_try_clone_missing_dropping_state_check.md](./2026-02-21_ISSUE_bug48_gc_try_clone_missing_dropping_state_check.md) | Gc::try_clone 缺少 dropping_state 檢查 - 與 try_deref 行為不一致 | Open | Not Verified |
| [2026-02-21_ISSUE_bug49_gc_ref_count_weak_count_doc_mismatch.md](./2026-02-21_ISSUE_bug49_gc_ref_count_weak_count_doc_mismatch.md) | Gc::ref_count() 與 Gc::weak_count() 文件與實作不符 - 文件說會 panic 但實際不會 | Open | Not Verified |
| [2026-02-21_ISSUE_bug50_gc_downgrade_missing_dead_check.md](./2026-02-21_ISSUE_bug50_gc_downgrade_missing_dead_check.md) | Gc::downgrade() 文件說會 panic 但實際不會 | Open | Not Verified |
| [2026-02-21_ISSUE_bug51_gchandle_downgrade_missing_dead_check.md](./2026-02-21_ISSUE_bug51_gchandle_downgrade_missing_dead_check.md) | GcHandle::downgrade() Missing Dead/Dropping State Check | Open | Not Verified |
| [2026-02-21_ISSUE_bug52_weak_strong_count_missing_dropping_check.md](./2026-02-21_ISSUE_bug52_weak_strong_count_missing_dropping_check.md) | Weak::strong_count() 與 Weak::weak_count() 缺少 dropping_state 檢查 | Open | Not Verified |
| [2026-02-21_ISSUE_bug53_gccell_borrow_mut_missing_satb_fallback.md](./2026-02-21_ISSUE_bug53_gccell_borrow_mut_missing_satb_fallback.md) | GcCell::borrow_mut() 缺少 SATB buffer overflow fallback 請求 | Open | Not Verified |
| [2026-02-21_ISSUE_bug54_gc_request_clear_relaxed_ordering.md](./2026-02-21_ISSUE_bug54_gc_request_clear_relaxed_ordering.md) | GC Request Clear 使用 Relaxed Ordering 導致執行緒可能錯過 GC 完成信號 | Open | Not Verified |
| [2026-02-21_ISSUE_bug55_asyncgchandle_downcast_ref_missing_dead_check.md](./2026-02-21_ISSUE_bug55_asyncgchandle_downcast_ref_missing_dead_check.md) | AsyncGcHandle::downcast_ref() 缺少 Dead Flag 檢查導致潛在 UAF | Open | Not Verified |
| [2026-02-21_ISSUE_bug56_gchandle_clone_missing_dead_check.md](./2026-02-21_ISSUE_bug56_gchandle_clone_missing_dead_check.md) | GcHandle::clone() Missing Dead Flag Check 導致潛在記憶體不安全 | Open | Not Verified |
| [2026-02-21_ISSUE_bug57_ephemeron_trace_always_traces_value.md](./2026-02-21_ISSUE_bug57_ephemeron_trace_always_traces_value.md) | Ephemeron<K,V> Trace 實作總是追蹤 value，導致記憶體無法正確回收 | Open | Not Verified |
| [2026-02-21_ISSUE_bug58_weak_is_alive_missing_dropping_state.md](./2026-02-21_ISSUE_bug58_weak_is_alive_missing_dropping_state.md) | Weak::is_alive() 缺少 dropping_state 檢查導致不一致行為 | Open | Not Verified |
| [2026-02-21_ISSUE_bug59_gcrwlock_write_guard_drop_missing_satb.md](./2026-02-21_ISSUE_bug59_gcrwlock_write_guard_drop_missing_satb.md) | GcRwLockWriteGuard 與 GcMutexGuard Drop 時缺少 SATB Barrier 標記 | Open | Not Verified |
