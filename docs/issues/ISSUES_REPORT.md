# Bug Issues Report

## Statistics

### By Status
- **Fixed**: 89
- **Open**: 29
- **Invalid**: 4
- **Verified**: 1

### By Tags
- **Verified**: 89
- **Not Verified**: 3
- **Not Reproduced**: 6
- **Unverified**: 25

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
| [2026-02-20_ISSUE_bug35_std_rwlock_capture_try_read.md](./2026-02-20_ISSUE_bug35_std_rwlock_capture_try_read.md) | std::sync::RwLock 的 GcCapture 實作使用 try_read() 可能導致指標遺漏 | Fixed | Verified |
| [2026-02-20_ISSUE_bug36_std_mutex_missing_gccapture.md](./2026-02-20_ISSUE_bug36_std_mutex_missing_gccapture.md) | std::sync::Mutex 缺少 GcCapture 實作導致指標遺漏 | Fixed | Verified |
| [2026-02-20_ISSUE_bug37_arc_missing_gccapture.md](./2026-02-20_ISSUE_bug37_arc_missing_gccapture.md) | std::sync::Arc 缺少 GcCapture 實作導致指標遺漏 | Fixed | Verified |
| [2026-02-20_ISSUE_bug38_rc_missing_gccapture.md](./2026-02-20_ISSUE_bug38_rc_missing_gccapture.md) | std::rc::Rc 缺少 GcCapture 實作導致 SATB 屏障失效 | Fixed | Verified |
| [2026-02-20_ISSUE_bug39_gchandle_resolve_missing_validity_check.md](./2026-02-20_ISSUE_bug39_gchandle_resolve_missing_validity_check.md) | GcHandle::resolve() 缺少物件有效性驗證 | Fixed | Verified |
| [2026-02-20_ISSUE_bug40_zst_singleton_ref_count.md](./2026-02-20_ISSUE_bug40_zst_singleton_ref_count.md) | ZST Singleton 初始化時 ref_count 為 2 而非 1 | Invalid | Not Verified |
| [2026-02-20_ISSUE_bug41_gcbox_weak_upgrade_dropping_state.md](./2026-02-20_ISSUE_bug41_gcbox_weak_upgrade_dropping_state.md) | GcBoxWeakRef::upgrade() 未檢查 dropping_state 導致 Use-After-Free 風險 | Fixed | Verified |
| [2026-02-20_ISSUE_bug42_weak_try_upgrade_missing_dropping_state.md](./2026-02-20_ISSUE_bug42_weak_try_upgrade_missing_dropping_state.md) | Weak::try_upgrade() 缺少 dropping_state 檢查導致 Use-After-Free 風險 | Fixed | Verified |
| [2026-02-20_ISSUE_bug43_weak_ephemeron_missing_gccapture.md](./2026-02-20_ISSUE_bug43_weak_ephemeron_missing_gccapture.md) | Weak<T> and Ephemeron<K,V> missing GcCapture implementation | Fixed | Verified |
| [2026-02-21_ISSUE_bug44_gc_clone_missing_flag_check.md](./2026-02-21_ISSUE_bug44_gc_clone_missing_flag_check.md) | Gc::clone() 缺少 has_dead_flag 和 dropping_state 檢查導致異常行為 | Fixed | Verified |
| [2026-02-21_ISSUE_bug45_dirty_pages_snapshot_race.md](./2026-02-21_ISSUE_bug45_dirty_pages_snapshot_race.md) | Dirty Pages Snapshot Race 導致 Young 物件被錯誤回收 | Fixed | Verified |
| [2026-02-21_ISSUE_bug46_gc_clone_missing_dead_flag_check.md](./2026-02-21_ISSUE_bug46_gc_clone_missing_dead_flag_check.md) | Gc::clone() Missing Dead Flag Check 導致記憶體不安全 | Fixed | Verified |
| [2026-02-21_ISSUE_bug47_gc_as_ptr_doc_mismatch.md](./2026-02-21_ISSUE_bug47_gc_as_ptr_doc_mismatch.md) | Gc::as_ptr() 文件與實作不符 - 文件說會 panic 但實際不會 | Fixed | Verified |
| [2026-02-21_ISSUE_bug48_gc_try_clone_missing_dropping_state_check.md](./2026-02-21_ISSUE_bug48_gc_try_clone_missing_dropping_state_check.md) | Gc::try_clone 缺少 dropping_state 檢查 - 與 try_deref 行為不一致 | Fixed | Verified |
| [2026-02-21_ISSUE_bug49_gc_ref_count_weak_count_doc_mismatch.md](./2026-02-21_ISSUE_bug49_gc_ref_count_weak_count_doc_mismatch.md) | Gc::ref_count() 與 Gc::weak_count() 文件與實作不符 - 文件說會 panic 但實際不會 | Fixed | Verified |
| [2026-02-21_ISSUE_bug50_gc_downgrade_missing_dead_check.md](./2026-02-21_ISSUE_bug50_gc_downgrade_missing_dead_check.md) | Gc::downgrade() 文件說會 panic 但實際不會 | Fixed | Verified |
| [2026-02-21_ISSUE_bug51_gchandle_downgrade_missing_dead_check.md](./2026-02-21_ISSUE_bug51_gchandle_downgrade_missing_dead_check.md) | GcHandle::downgrade() Missing Dead/Dropping State Check | Fixed | Verified |
| [2026-02-21_ISSUE_bug52_weak_strong_count_missing_dropping_check.md](./2026-02-21_ISSUE_bug52_weak_strong_count_missing_dropping_check.md) | Weak::strong_count() 與 Weak::weak_count() 缺少 dropping_state 檢查 | Fixed | Verified |
| [2026-02-21_ISSUE_bug53_gccell_borrow_mut_missing_satb_fallback.md](./2026-02-21_ISSUE_bug53_gccell_borrow_mut_missing_satb_fallback.md) | GcCell::borrow_mut() 缺少 SATB buffer overflow fallback 請求 | Fixed | Verified |
| [2026-02-21_ISSUE_bug54_gc_request_clear_relaxed_ordering.md](./2026-02-21_ISSUE_bug54_gc_request_clear_relaxed_ordering.md) | GC Request Clear 使用 Relaxed Ordering 導致執行緒可能錯過 GC 完成信號 | Fixed | Verified |
| [2026-02-21_ISSUE_bug55_asyncgchandle_downcast_ref_missing_dead_check.md](./2026-02-21_ISSUE_bug55_asyncgchandle_downcast_ref_missing_dead_check.md) | AsyncGcHandle::downcast_ref() 缺少 Dead Flag 檢查導致潛在 UAF | Fixed | Verified |
| [2026-02-21_ISSUE_bug56_gchandle_clone_missing_dead_check.md](./2026-02-21_ISSUE_bug56_gchandle_clone_missing_dead_check.md) | GcHandle::clone() Missing Dead Flag Check 導致潛在記憶體不安全 | Fixed | Verified |
| [2026-02-21_ISSUE_bug57_ephemeron_trace_always_traces_value.md](./2026-02-21_ISSUE_bug57_ephemeron_trace_always_traces_value.md) | Ephemeron<K,V> Trace 實作總是追蹤 value，導致記憶體無法正確回收 | Fixed | Verified |
| [2026-02-21_ISSUE_bug58_weak_is_alive_missing_dropping_state.md](./2026-02-21_ISSUE_bug58_weak_is_alive_missing_dropping_state.md) | Weak::is_alive() 缺少 dropping_state 檢查導致不一致行為 | Fixed | Verified |
| [2026-02-21_ISSUE_bug59_gcrwlock_write_guard_drop_missing_satb.md](./2026-02-21_ISSUE_bug59_gcrwlock_write_guard_drop_missing_satb.md) | GcRwLockWriteGuard 與 GcMutexGuard Drop 時缺少 SATB Barrier 標記 | Fixed | Verified |
| [2026-02-21_ISSUE_bug60_mark_page_dirty_for_ptr_large_object.md](./2026-02-21_ISSUE_bug60_mark_page_dirty_for_ptr_large_object.md) | mark_page_dirty_for_ptr 未處理大型物件導致 Vec<Gc<T>> 追蹤失敗 | Fixed | Verified |
| [2026-02-21_ISSUE_bug61_gcmutex_capture_gc_ptrs_try_lock.md](./2026-02-21_ISSUE_bug61_gcmutex_capture_gc_ptrs_try_lock.md) | GcMutex::capture_gc_ptrs_into() 使用 try_lock() 而非 lock()，與 GcRwLock 不一致 | Fixed | Verified |
| [2026-02-21_ISSUE_bug62_gchandle_resolve_dropping_state.md](./2026-02-21_ISSUE_bug62_gchandle_resolve_dropping_state.md) | GcHandle::resolve() 與 GcHandle::try_resolve() 缺少 dropping_state 檢查 | Fixed | Verified |
| [2026-02-21_ISSUE_bug63_cross_thread_handle_missing_dead_check.md](./2026-02-21_ISSUE_bug63_cross_thread_handle_missing_dead_check.md) | Gc::cross_thread_handle() 與 Gc::weak_cross_thread_handle() 缺少 dead_flag / dropping_state 檢查 | Verified | Verified |
| [2026-02-22_ISSUE_bug64_weak_clone_missing_dead_check.md](./2026-02-22_ISSUE_bug64_weak_clone_missing_dead_check.md) | Weak::clone() 缺少 dead_flag / dropping_state 檢查 | Fixed | Verified |
| [2026-02-22_ISSUE_bug65_gcrwlock_gcmutex_write_missing_satb_old_value.md](./2026-02-22_ISSUE_bug65_gcrwlock_gcmutex_write_missing_satb_old_value.md) | GcRwLock 與 GcMutex 的 write()/lock() 缺少 SATB 舊值捕獲，導致增量標記期間潛在 UAF | Fixed | Verified |
| [2026-02-22_ISSUE_bug66_parking_lot_mutex_rwlock_missing_gccapture.md](./2026-02-22_ISSUE_bug66_parking_lot_mutex_rwlock_missing_gccapture.md) | parking_lot::Mutex 與 parking_lot::RwLock 缺少 GcCapture 實作導致指標遺漏 | Fixed | Verified |
| [2026-02-22_ISSUE_bug67_vecdeque_linkedlist_missing_gccapture.md](./2026-02-22_ISSUE_bug67_vecdeque_linkedlist_missing_gccapture.md) | VecDeque 與 LinkedList 缺少 GcCapture 實作導致指標遺漏 | Fixed | Verified |
| [2026-02-22_ISSUE_bug68_gc_as_weak_missing_dead_check.md](./2026-02-22_ISSUE_bug68_gc_as_weak_missing_dead_check.md) | Gc::as_weak() 缺少 dead_flag / dropping_state 檢查 | Fixed | Verified |
| [2026-02-22_ISSUE_bug69_gcboxweakref_clone_missing_dead_check.md](./2026-02-22_ISSUE_bug69_gcboxweakref_clone_missing_dead_check.md) | GcBoxWeakRef::clone() 缺少 dead_flag / dropping_state 檢查 | Fixed | Verified |
| [2026-02-22_ISSUE_bug70_asynchandle_to_gc_missing_dead_check.md](./2026-02-22_ISSUE_bug70_asynchandle_to_gc_missing_dead_check.md) | AsyncHandle::to_gc 缺少 dead_flag / dropping_state 檢查，與 Handle::to_gc 行為不一致 | Fixed | Verified |
| [2026-02-22_ISSUE_bug71_write_barrier_gen_old_flag_page_generation_mismatch.md](./2026-02-22_ISSUE_bug71_write_barrier_gen_old_flag_page_generation_mismatch.md) | Write Barrier 僅檢查 per-object GEN_OLD_FLAG 忽略 Page Generation 導致 OLD→YOUNG 引用遺漏 | Fixed | Verified |
| [2026-02-22_ISSUE_bug72_gchandle_resolve_unregistered_handle_ub.md](./2026-02-22_ISSUE_bug72_gchandle_resolve_unregistered_handle_ub.md) | GcHandle::resolve() / try_resolve() 未檢查 handle_id 是否已失效 | Fixed | Verified |
| [2026-02-22_ISSUE_bug73_incremental_transition_to_toctou.md](./2026-02-22_ISSUE_bug73_incremental_transition_to_toctou.md) | IncrementalMarkState::transition_to has TOCTOU Race Condition | Fixed | Verified |
| [2026-02-22_ISSUE_bug74_handle_get_missing_dead_check.md](./2026-02-22_ISSUE_bug74_handle_get_missing_dead_check.md) | Handle::get() / AsyncHandle::get() 缺少 dead_flag / dropping_state 檢查導致潛在 UAF | Fixed | Verified |
| [2026-02-22_ISSUE_bug75_gcboxweakref_upgrade_refcount_leak.md](./2026-02-22_ISSUE_bug75_gcboxweakref_upgrade_refcount_leak.md) | GcBoxWeakRef::upgrade ref_count leak due to TOCTOU between try_inc_ref_from_zero and dropping_state check | Fixed | Verified |
| [2026-02-22_ISSUE_bug76_ephemeron_clone_null_value.md](./2026-02-22_ISSUE_bug76_ephemeron_clone_null_value.md) | Ephemeron::clone() creates null value Gc when original value is dead/dropping | Fixed | Verified |
| [2026-02-23_ISSUE_bug77_lazy_sweep_infinite_loop.md](./2026-02-23_ISSUE_bug77_lazy_sweep_infinite_loop.md) | Lazy Sweep 發生無窮迴圈 - is_allocated 為 true 時 continue 導致無限循環 | Fixed | Verified |
| [2026-02-23_ISSUE_bug78_parallel_marking_missing_is_allocated_check.md](./2026-02-23_ISSUE_bug78_parallel_marking_missing_is_allocated_check.md) | Parallel Marking 缺少 is_allocated 檢查 - 可能標記錯誤物件 | Fixed | Verified |
| [2026-02-23_ISSUE_bug79_vecdeque_linkedlist_missing_gccapture.md](./2026-02-23_ISSUE_bug79_vecdeque_linkedlist_missing_gccapture.md) | VecDeque 與 LinkedList 缺少 GcCapture 實作導致指標遺漏 | Fixed | Verified |
| [2026-02-23_ISSUE_bug80_asynchandle_togc_missing_refcount_increment.md](./2026-02-23_ISSUE_bug80_asynchandle_togc_missing_refcount_increment.md) | AsyncHandle::to_gc 缺少 ref count 增量導致 Use-After-Free | Fixed | Verified |
| [2026-02-23_ISSUE_bug81_async_handle_to_gc_uaf.md](./2026-02-23_ISSUE_bug81_async_handle_to_gc_uaf.md) | AsyncHandle::to_gc 缺少 ref count 增量與 dead check 導致 UAF | Fixed | Verified |
| [2026-02-23_ISSUE_bug82_binaryheap_missing_gccapture.md](./2026-02-23_ISSUE_bug82_binaryheap_missing_gccapture.md) | BinaryHeap 缺少 GcCapture 實作導致指標遺漏 | Fixed | Verified |
| [2026-02-23_ISSUE_bug83_gchandle_resolve_toctou_race.md](./2026-02-23_ISSUE_bug83_gchandle_resolve_toctou_race.md) | GcHandle resolve/clone 存在 TOCTOU Race Condition 導致 Use-After-Free | Fixed | Verified |
| [2026-02-23_ISSUE_bug84_parallel_marking_worker_index.md](./2026-02-23_ISSUE_bug84_parallel_marking_worker_index.md) | Parallel Marking Worker Index Uses Wrong Pointer | Fixed | Verified |
| [2026-02-23_ISSUE_bug85_refcell_gccapture_borrow_panic.md](./2026-02-23_ISSUE_bug85_refcell_gccapture_borrow_panic.md) | RefCell GcCapture 使用 borrow() 可能導致 panic | Fixed | Verified |
| [2026-02-23_ISSUE_bug86_refcell_gccapture_borrow_panic.md](./2026-02-23_ISSUE_bug86_refcell_gccapture_borrow_panic.md) | RefCell GcCapture 使用 borrow() 導致 panic | Fixed | Verified |
| [2026-02-23_ISSUE_bug87_binaryheap_missing_trace.md](./2026-02-23_ISSUE_bug87_binaryheap_missing_trace.md) | BinaryHeap 缺少 Trace 與 GcCapture 實作導致無法與 Gc 整合 | Fixed | Verified |
| [2026-02-23_ISSUE_bug88_cow_missing_trace_gccapture.md](./2026-02-23_ISSUE_bug88_cow_missing_trace_gccapture.md) | std::borrow::Cow 缺少 Trace 與 GcCapture 實作導致無法與 Gc 整合 | Fixed | Verified |
| [2026-02-23_ISSUE_bug89_gc_clone_missing_is_under_construction_check.md](./2026-02-23_ISSUE_bug89_gc_clone_missing_is_under_construction_check.md) | Gc::clone() 缺少 is_under_construction 檢查 - 與其他操作不一致 | Fixed | Verified |
| [2026-02-23_ISSUE_bug90_async_handle_slot_allocation_race.md](./2026-02-23_ISSUE_bug90_async_handle_slot_allocation_race.md) | AsyncHandleScope slot allocation race condition - TOCTOU between fetch_add and bounds check | Fixed | Verified |
| [2026-02-24_ISSUE_bug91_gcbox_inc_weak_race_condition.md](./2026-02-24_ISSUE_bug91_gcbox_inc_weak_race_condition.md) | GcBox::inc_weak 使用 load+store 導致並發調用時 weak_count 丢失更新 | Fixed | Verified |
| [2026-02-24_ISSUE_bug92_gc_downgrade_missing_is_under_construction_check.md](./2026-02-24_ISSUE_bug92_gc_downgrade_missing_is_under_construction_check.md) | Gc::downgrade() 缺少 is_under_construction 檢查 - 與 Gc::clone() 行為不一致 | Open | Unverified |
| [2026-02-24_ISSUE_bug93_slot_reuse_dead_flag_not_cleared.md](./2026-02-24_ISSUE_bug93_slot_reuse_dead_flag_not_cleared.md) | Slot Reuse 時未清除 DEAD_FLAG 導致新物件被錯誤標記為死亡 | Open | Unverified |
| [2026-02-24_ISSUE_bug94_gc_deref_missing_is_under_construction_check.md](./2026-02-24_ISSUE_bug94_gc_deref_missing_is_under_construction_check.md) | Gc::deref() 和 Gc::try_deref() 缺少 is_under_construction 檢查 | Open | Unverified |
| [2026-02-24_ISSUE_bug95_gc_ref_count_weak_count_missing_is_under_construction_check.md](./2026-02-24_ISSUE_bug95_gc_ref_count_weak_count_missing_is_under_construction_check.md) | Gc::ref_count() 和 Gc::weak_count() 缺少 is_under_construction 檢查 | Open | Unverified |
| [2026-02-24_ISSUE_bug96_ephemeron_gccapture_missing_key_alive_check.md](./2026-02-24_ISSUE_bug96_ephemeron_gccapture_missing_key_alive_check.md) | Ephemeron GcCapture 實現不一致 - 未檢查 key 是否存活 | Open | Unverified |
| [2026-02-24_ISSUE_bug97_async_handle_to_gc_missing_ref_count.md](./2026-02-24_ISSUE_bug97_async_handle_to_gc_missing_ref_count.md) | AsyncHandle::to_gc() 漏增引用計數導致雙重釋放 | Open | Verified |
| [2026-02-24_ISSUE_bug98_generational_barrier_disabled_incremental.md](./2026-02-24_ISSUE_bug98_generational_barrier_disabled_incremental.md) | is_generational_barrier_active() returns false when incremental marking disabled, breaking GcRwLock/GcThreadSafeCell barriers | Open | Verified |
| [2026-02-24_ISSUE_bug99_async_gchandle_downcast_ref_missing_is_under_construction_check.md](./2026-02-24_ISSUE_bug99_async_gchandle_downcast_ref_missing_is_under_construction_check.md) | AsyncGcHandle::downcast_ref() 缺少 is_under_construction 檢查 - Bug55 修復不完整 | Open | Unverified |
| [2026-02-24_ISSUE_bug100_gc_cross_thread_handle_missing_is_under_construction_check.md](./2026-02-24_ISSUE_bug100_gc_cross_thread_handle_missing_is_under_construction_check.md) | Gc::cross_thread_handle() 缺少 is_under_construction 檢查 - Bug92 修復不完整 | Open | Unverified |
| [2026-02-24_ISSUE_bug100_trigger_write_barrier_toctou.md](./2026-02-24_ISSUE_bug100_trigger_write_barrier_toctou.md) | trigger_write_barrier TOCTOU - is_incremental_marking_active called twice | Open | Not Verified |
| [2026-02-24_ISSUE_bug101_sync_trigger_write_barrier_toctou.md](./2026-02-24_ISSUE_bug101_sync_trigger_write_barrier_toctou.md) | sync.rs trigger_write_barrier TOCTOU - is_incremental_marking_active called twice | Open | Unverified |
| [2026-02-24_ISSUE_bug102_async_handle_get_missing_checks.md](./2026-02-24_ISSUE_bug102_async_handle_get_missing_checks.md) | AsyncHandle::get() missing dead/dropping/construction checks | Fixed | Verified |
| [2026-02-24_ISSUE_bug103_gchandle_inc_ref_toctou_race.md](./2026-02-24_ISSUE_bug103_gchandle_inc_ref_toctou_race.md) | GcHandle/GcBoxWeakRef inc_ref TOCTOU Race - 檢查與遞增非原子操作導致 Use-After-Free | Open | Unverified |
| [2026-02-24_ISSUE_bug104_weak_clone_missing_is_under_construction_check.md](./2026-02-24_ISSUE_bug104_weak_clone_missing_is_under_construction_check.md) | Weak::clone() 和 GcBoxWeakRef::clone() 缺少 is_under_construction 檢查 - Bug64/69 修復不完整 | Open | Unverified |
| [2026-02-24_ISSUE_bug105_gcbox_as_weak_missing_is_under_construction_check.md](./2026-02-24_ISSUE_bug105_gcbox_as_weak_missing_is_under_construction_check.md) | GcBox::as_weak() 缺少 is_under_construction 檢查 - 內部方法缺少一致性檢查 | Open | Unverified |
| [2026-02-25_ISSUE_bug106_ephemeron_upgrade_toctou.md](./2026-02-25_ISSUE_bug106_ephemeron_upgrade_toctou.md) | Ephemeron::upgrade() TOCTOU race condition - key alive check and value clone not atomic | Open | Unverified |
| [2026-02-25_ISSUE_bug107_gcrwlock_guarded_drop_missing_generational_barrier.md](./2026-02-25_ISSUE_bug107_gcrwlock_guarded_drop_missing_generational_barrier.md) | GcRwLockWriteGuard/GcMutexGuard Drop 缺少 Generational Barrier 檢查 | Open | Unverified |
| [2026-02-25_ISSUE_bug108_gcboxweakref_clone_missing_safety_checks.md](./2026-02-25_ISSUE_bug108_gcboxweakref_clone_missing_safety_checks.md) | GcBoxWeakRef::clone 缺少安全檢查導致潛在 Use-After-Free | Open | Unverified |
| [2026-02-25_ISSUE_bug108_mark_new_object_black_missing_is_allocated_check.md](./2026-02-25_ISSUE_bug108_mark_new_object_black_missing_is_allocated_check.md) | mark_new_object_black 缺少 is_allocated 檢查，與 mark_object_black 行為不一致 | Fixed | Verified |
| [2026-02-25_ISSUE_bug109_gcthreadsaferefmut_drop_missing_generational_barrier.md](./2026-02-25_ISSUE_bug109_gcthreadsaferefmut_drop_missing_generational_barrier.md) | GcThreadSafeRefMut::drop 缺少 Generational Barrier 檢查 | Fixed | Verified |
| [2026-02-25_ISSUE_bug110_gccell_borrow_mut_triple_is_incremental_check.md](./2026-02-25_ISSUE_bug110_gccell_borrow_mut_triple_is_incremental_check.md) | GcCell::borrow_mut 三次調用 is_incremental_marking_active 導致 TOCTOU | Open | Unverified |
| [2026-02-25_ISSUE_bug111_gcthreadsafecell_trigger_write_barrier_toctou.md](./2026-02-25_ISSUE_bug111_gcthreadsafecell_trigger_write_barrier_toctou.md) | GcThreadSafeCell::trigger_write_barrier TOCTOU - is_incremental_marking_active called twice | Open | Unverified |
| [2026-02-25_ISSUE_bug112_try_inc_ref_from_zero_doc_mismatch.md](./2026-02-25_ISSUE_bug112_try_inc_ref_from_zero_doc_mismatch.md) | try_inc_ref_from_zero 文檔與實作不一致 - 聲稱檢查 "fully alive" 但只檢查 dead | Open | Unverified |
| [2026-02-25_ISSUE_bug113_gcbox_is_under_construction_relaxed_ordering.md](./2026-02-25_ISSUE_bug113_gcbox_is_under_construction_relaxed_ordering.md) | GcBox::is_under_construction() 使用 Relaxed Ordering 導致潜在 Race Condition | Open | Unverified |
| [2026-02-25_ISSUE_bug114_gc_cell_validate_and_barrier_gen_old_flag_toctou.md](./2026-02-25_ISSUE_bug114_gc_cell_validate_and_barrier_gen_old_flag_toctou.md) | gc_cell_validate_and_barrier GEN_OLD_FLAG 檢查與 barrier 執行之間存在 TOCTOU | Open | Unverified |
| [2026-02-25_ISSUE_bug115_async_handle_missing_scope_validity_check.md](./2026-02-25_ISSUE_bug115_async_handle_missing_scope_validity_check.md) | AsyncHandle 缺少 scope 有效性檢查導致 use-after-free | Open | Verified |
| [2026-02-25_ISSUE_bug116_gcthreadsafecell_borrow_mut_toctou.md](./2026-02-25_ISSUE_bug116_gcthreadsafecell_borrow_mut_toctou.md) | GcThreadSafeCell::borrow_mut() TOCTOU - 兩處 is_incremental_marking_active() 調用導致狀態不一致 | Open | Unverified |
| [2026-02-25_ISSUE_bug117_weak_strong_count_missing_is_under_construction_check.md](./2026-02-25_ISSUE_bug117_weak_strong_count_missing_is_under_construction_check.md) | Weak::strong_count() 和 Weak::weak_count() 缺少 is_under_construction 檢查 - 與 Weak::upgrade 行為不一致 | Open | Unverified |
| [2026-02-25_ISSUE_bug118_write_guard_drop_toctou.md](./2026-02-25_ISSUE_bug118_write_guard_drop_toctou.md) | Write Guard Drop TOCTOU - 檢查 barrier 狀態與調用 mark_object_black 之间状态可能改变 | Open | Unverified |
| [2026-02-26_ISSUE_bug119_weak_upgrade_toctou_dropping_cas_race.md](./2026-02-26_ISSUE_bug119_weak_upgrade_toctou_dropping_cas_race.md) | GcBoxWeakRef::upgrade TOCTOU - dropping_state 檢查與 try_inc_ref_from_zero CAS 之間的 Race 導致 Use-After-Free | Open | Unverified |
| [2026-02-26_ISSUE_bug120_gcboxweakref_try_upgrade_toctou.md](./2026-02-26_ISSUE_bug120_gcboxweakref_try_upgrade_toctou.md) | GcBoxWeakRef::try_upgrade TOCTOU - is_dead_or_unrooted 檢查與 inc_ref人之間的 Race 導致 Use-After-Free | Open | Unverified |
| [2026-02-26_ISSUE_bug121_gcbox_dec_weak_count_zero.md](./2026-02-26_ISSUE_bug121_gcbox_dec_weak_count_zero.md) | GcBox::dec_weak 當 weak_count 為 0 時錯誤地返回 true - 與 Weak::drop 行為不一致 | Open | Unverified |
