# Parallel Marking 技術規格 Phase 2：整合與優化

## 1. 現況分析

### A. 已完成項目 (Phase 1)
基於 `parallel-marking-plan-1.md` 與 ChezScheme 的設計，Phase 1 已實作：

1. **BiBOP 頁面所有權**：`PageHeader.owner_id` 標記每個頁面的擁有者執行緒
2. **執行緒控制塊**：`ThreadControlBlock` 包含 `local_mark_stack` 和 `remote_inbox`
3. **遠端轉發邏輯**：`GcVisitor.visit_with_ownership()` 實現所有權檢查與訊息轉發
4. **原型 Worker 函式**：`parallel_mark_worker()` 和 `perform_parallel_marking()` 已可在測試中運行

### B. 待解決問題
1. `perform_parallel_marking` 尚未整合進 `collect()` 主流程
2. 每次 GC 都 `spawn` 新執行緒，效能開銷大
3. 終止檢測邏輯未完善，可能在高併發下失效
4. 與現有 STW 協調機制尚未對接

---

## 2. Phase 2 目標

將並行標記從「測試可用」升級為「生產就緒」，具體包括：

| 項目 | 說明 |
|------|------|
| Worker Pool | 預先建立的 Sweeper 執行緒池，GC 時喚醒 |
| STW 整合 | 與現有 `request_gc_handshake()` 機制對接 |
| 終止檢測 | 實作 ChezScheme 風格的 Barrier + Work-checking |
| 主流程整合 | `collect()` 根據執行緒數動態選擇並行/單執行緒 |

---

## 3. 數據結構變更

### A. 新增 `SweepWorker` 結構

```rust
/// 並行標記的 Worker 執行緒控制結構
pub struct SweepWorker {
    /// Worker 執行緒的 ID (0..num_workers)
    pub id: usize,
    /// 執行緒狀態
    pub status: AtomicUsize,
    /// 條件變數：等待工作
    pub work_cond: Condvar,
    /// 條件變數：通知完成
    pub done_cond: Condvar,
    /// 狀態鎖
    pub mutex: Mutex<()>,
    /// 關聯的 ThreadControlBlock 列表
    pub tcbs: Vec<Arc<ThreadControlBlock>>,
}

/// Worker 狀態常量 (對應 ChezScheme)
pub const SWEEPER_NONE: usize = 0;
pub const SWEEPER_READY: usize = 1;
pub const SWEEPER_SWEEPING: usize = 2;
pub const SWEEPER_WAITING_FOR_WORK: usize = 3;
```

### B. 擴充 `ThreadRegistry`

```rust
impl ThreadRegistry {
    /// 全域 Worker Pool
    pub sweep_workers: Vec<Arc<SweepWorker>>,
    /// 正在運行的 Worker 數量
    pub num_running_sweepers: AtomicUsize,
    /// 全域掃描互斥鎖
    pub sweep_mutex: Mutex<()>,
}
```

### C. 擴充 `ThreadControlBlock`

```rust
impl ThreadControlBlock {
    /// 發送緩衝區（避免頻繁加鎖）
    pub send_buffer: UnsafeCell<Vec<(*const u8, usize)>>, // (ptr, target_worker_id)
    /// 接收緩衝區
    pub receive_buffer: UnsafeCell<Vec<*const u8>>,
}
```

---

## 4. 核心演算法

### A. Worker Pool 初始化

在 `ThreadRegistry::new()` 時根據 CPU 核心數建立 Worker Pool：

```rust
fn initialize_worker_pool(num_workers: usize) {
    for i in 0..num_workers {
        let worker = Arc::new(SweepWorker::new(i));
        std::thread::spawn({
            let worker = Arc::clone(&worker);
            move || sweeper_thread_main(worker)
        });
        registry.sweep_workers.push(worker);
    }
}

fn sweeper_thread_main(worker: Arc<SweepWorker>) {
    loop {
        // 等待 GC 啟動信號
        wait_for_gc_start(&worker);
        
        if worker.should_shutdown() {
            break;
        }
        
        // 執行標記工作
        run_sweeper(&worker);
        
        // 通知完成
        signal_done(&worker);
    }
}
```

### B. 主流程整合

修改 `gc.rs::perform_multi_threaded_collect()`：

```rust
fn perform_multi_threaded_collect() {
    let registry = thread_registry().lock().unwrap();
    let num_threads = registry.threads.len();
    
    // 階段 1：清除所有標記
    for tcb in &registry.threads {
        clear_all_marks_and_dirty(unsafe { &*tcb.heap.get() });
    }
    
    // 階段 2：選擇標記策略
    if num_threads > 1 && cfg!(feature = "parallel-gc") {
        // === 並行標記 ===
        perform_parallel_marking_integrated(&registry);
    } else {
        // === 單執行緒標記 ===
        for tcb in &registry.threads {
            mark_major_roots_multi(unsafe { &mut *tcb.heap.get() }, &all_stack_roots);
        }
    }
    
    // 階段 3：清除未標記物件
    for tcb in &registry.threads {
        sweep_segment_pages(unsafe { &*tcb.heap.get() }, false);
        sweep_large_objects(unsafe { &mut *tcb.heap.get() }, false);
    }
}
```

### C. 並行標記整合版

```rust
fn perform_parallel_marking_integrated(registry: &ThreadRegistry) {
    // 1. 分配執行緒到 Workers
    distribute_tcbs_to_workers(registry);
    
    // 2. 收集所有 Stack Roots
    let all_roots = collect_all_stack_roots(registry);
    
    // 3. 初始化各 Worker 的 Local Mark Stack
    for worker in &registry.sweep_workers {
        for tcb in &worker.tcbs {
            initialize_roots_for_tcb(tcb, &all_roots);
        }
        worker.status.store(SWEEPER_READY, Ordering::Release);
    }
    
    // 4. 啟動所有 Workers
    registry.num_running_sweepers.store(
        registry.sweep_workers.len(), 
        Ordering::SeqCst
    );
    
    for worker in &registry.sweep_workers {
        worker.work_cond.notify_one();
    }
    
    // 5. 等待所有 Workers 完成
    wait_for_all_workers_done(registry);
}
```

---

## 5. 終止檢測 (Termination Detection)

採用 ChezScheme 的「運行計數 + 工作檢查」策略：

### A. 核心邏輯

```rust
fn run_sweeper(worker: &SweepWorker) {
    loop {
        // 處理本地工作
        for tcb in &worker.tcbs {
            sweep_tcb_local_work(tcb);
            flush_send_buffer(tcb);
        }
        
        // 嘗試接收遠端工作
        let mut has_remote_work = false;
        for tcb in &worker.tcbs {
            if drain_receive_buffer(tcb) > 0 {
                has_remote_work = true;
            }
        }
        
        if has_remote_work {
            continue; // 有新工作，繼續處理
        }
        
        // 進入等待/終止檢測
        let _guard = SWEEP_MUTEX.lock().unwrap();
        NUM_RUNNING_SWEEPERS.fetch_sub(1, Ordering::SeqCst);
        
        // 再次檢查是否有工作（避免競態）
        let mut any_pending = false;
        for tcb in &worker.tcbs {
            if !tcb.receive_buffer_empty() {
                any_pending = true;
                break;
            }
        }
        
        if NUM_RUNNING_SWEEPERS.load(Ordering::SeqCst) == 0 && !any_pending {
            // 全域終止：喚醒所有等待的 Workers
            for w in &ALL_WORKERS {
                w.work_cond.notify_all();
            }
            break;
        } else if any_pending {
            // 有新工作出現，恢復運行
            NUM_RUNNING_SWEEPERS.fetch_add(1, Ordering::SeqCst);
        } else {
            // 等待其他 Worker 的轉發
            worker.status.store(SWEEPER_WAITING_FOR_WORK, Ordering::Release);
            worker.work_cond.wait(&_guard);
            
            if worker.status.load(Ordering::Acquire) != SWEEPER_WAITING_FOR_WORK {
                // 被喚醒處理新工作
            } else if NUM_RUNNING_SWEEPERS.load(Ordering::SeqCst) == 0 {
                // 全域終止
                break;
            }
        }
    }
}
```

### B. 遠端轉發優化

參考 ChezScheme 的 `send_and_receive_remote_sweeps()`：

```rust
fn flush_send_buffer(tcb: &ThreadControlBlock) {
    let _guard = SWEEP_MUTEX.lock().unwrap();
    
    let buffer = unsafe { &mut *tcb.send_buffer.get() };
    for (ptr, target_worker_id) in buffer.drain(..) {
        let target_worker = &ALL_WORKERS[target_worker_id];
        let target_tcb = &target_worker.tcbs[0]; // 簡化：每個 worker 一個 tcb
        
        unsafe {
            (*target_tcb.receive_buffer.get()).push(ptr);
        }
        
        // 如果目標正在等待，喚醒它
        if target_worker.status.load(Ordering::Acquire) == SWEEPER_WAITING_FOR_WORK {
            NUM_RUNNING_SWEEPERS.fetch_add(1, Ordering::SeqCst);
            target_worker.status.store(SWEEPER_SWEEPING, Ordering::Release);
            target_worker.work_cond.notify_one();
        }
    }
}
```

---

## 6. 效能優化策略

### A. 批次轉發 (Batched Forwarding)
- 累積 N 個遠端指標後一次性加鎖轉發
- 推薦 N = 64，可根據 Cache Line 調整

### B. Local-first 策略
- 在訪問 `remote_inbox` 前優先處理完所有 `local_mark_stack`
- 減少鎖爭用頻率

### C. 無鎖接收緩衝區 (可選)
使用 `crossbeam::queue::SegQueue` 替代 `Mutex<Vec>`：

```rust
pub remote_inbox: SegQueue<*const u8>,
```

### D. 頁面預取 (Prefetching)
在標記物件前預取下一個物件的記憶體：

```rust
while let Some(ptr) = local_stack.pop() {
    if let Some(next_ptr) = local_stack.peek() {
        prefetch_read(next_ptr);
    }
    process_object(ptr);
}
```

---

## 7. 實作順序

### Phase 2.1：Worker Pool 基礎設施
1. 實作 `SweepWorker` 結構
2. 修改 `ThreadRegistry` 建立 Worker Pool
3. 測試 Worker 的啟動/等待/喚醒機制

### Phase 2.2：終止檢測
1. 實作 `NUM_RUNNING_SWEEPERS` 計數邏輯
2. 實作 Condvar 等待/喚醒協議
3. 建立終止檢測的單元測試

### Phase 2.3：主流程整合
1. 修改 `perform_multi_threaded_collect()` 調用並行標記
2. 確保與 STW 機制正確對接
3. 整合測試：多執行緒程式的 GC 正確性

### Phase 2.4：效能調優
1. 實作批次轉發
2. 基準測試與分析
3. 調整參數（批次大小、Worker 數量）

---

## 8. 測試計劃

### A. 正確性測試
```rust
#[test]
fn test_parallel_marking_cross_thread_references() {
    // 建立多執行緒，每個執行緒分配物件
    // 建立跨執行緒的引用環
    // 觸發 GC，驗證所有可達物件存活
}

#[test]
fn test_parallel_marking_termination() {
    // 測試當所有 Worker 同時完成時的終止邏輯
    // 測試當 Worker 互相等待時的死鎖避免
}
```

### B. 壓力測試
```rust
#[test]
fn stress_parallel_gc_high_contention() {
    // 8+ 執行緒，每個執行緒高頻分配
    // 大量跨執行緒引用
    // 運行 1000+ 次 GC 週期
}
```

### C. 效能基準
```rust
#[bench]
fn bench_parallel_vs_single_thread_marking() {
    // 比較並行與單執行緒標記的吞吐量
    // 測量不同執行緒數的 Scaling 效率
}
```

---

## 9. ChezScheme 參考對照表

| ChezScheme | rudo-gc | 說明 |
|------------|---------|------|
| `thread_gc` | `ThreadControlBlock` | 執行緒 GC 狀態 |
| `seginfo.creator` | `PageHeader.owner_id` | 頁面所有權 |
| `send_remote_sweep_stack` | `send_buffer` | 發送緩衝區 |
| `receive_remote_sweep_stack` | `receive_buffer` / `remote_inbox` | 接收緩衝區 |
| `gc_sweeper` | `SweepWorker` | Worker 結構 |
| `SWEEPER_WAITING_FOR_WORK` | `SWEEPER_WAITING_FOR_WORK` | 等待狀態 |
| `num_running_sweepers` | `num_running_sweepers` | 運行計數 |
| `sweep_mutex` | `SWEEP_MUTEX` | 全域掃描鎖 |

---

## 10. 風險與緩解

| 風險 | 緩解措施 |
|------|----------|
| 死鎖 | 單一全域鎖 + 嚴格的加鎖順序 |
| 飢餓 | 工作竊取 (Work-stealing) 作為備援 |
| 記憶體序錯誤 | SeqCst 用於關鍵狀態變更，Miri 驗證 |
| 效能退化 | Feature flag 控制，可隨時回退單執行緒 |

---

## 11. McCarthy 的提醒

> 「並行 GC 的陷阱在於你以為問題出在演算法，其實問題出在時序。每個 Worker 都必須清楚地知道：什麼時候該工作，什麼時候該等待，什麼時候該收工。ChezScheme 的成功在於它用一個簡單的計數器就解決了這三個問題——正在運行的 Sweeper 數量。當它歸零且沒有待處理訊息時，大家就可以回家了。」
