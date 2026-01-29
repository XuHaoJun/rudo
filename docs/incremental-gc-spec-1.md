# rudo-gc 增量收集器技術規格文件

## 版本資訊

| 項目 | 內容 |
|------|------|
| 文件版本 | v1.0 |
| 日期 | 2026-01-29 |
| 作者 | rudo-gc 開發團隊 |
| 參考 | R. Kent Dybvig, John McCarthy |
| 目標 | 實現增量標記與增量清除以降低 GC 暫停時間 |

---

## 1. 設計目標與非目標

### 1.1 設計目標

1. **正確性優先**：在任何情況下不遺漏標記、不釋放仍在使用的記憶體
2. **暫停時間限制**：99.9% 的 GC 暫停不超過 5ms
3. **漸進式實作**：分階段交付，每階段可獨立運作
4. **向後相容**：現有 API 和行為保持不變

### 1.2 非目標

1. **併發標記**：暫時不實現（Phase 4+）
2. **移動式收集**：保持非移動設計
3. **即時 GC**：不承諾硬即時保證

---

## 2. 系統架構總覽

### 2.1 現有架構與增量擴展

```
+---------------------------------------------------------------+
|                      rudo-gc 架構圖                           |
+---------------------------------------------------------------+
|                                                               |
|   +-------------+     +-------------+     +-------------+     |
|   |  Mutator    |     |  Mutator    |     |  Mutator    |     |
|   |  (執行中)    |     |  (執行中)    |     |  (執行中)    |     |
|   +------+------+     +------+------+     +------+------+     |
|          |                   |                   |             |
|          +-------------------+-------------------+             |
|                              |                                 |
|                   +----------------------+                     |
|                   |  Safepoint 檢查      |                     |
|                   |  (每個安全點)         |                     |
|                   +----------+-----------+                     |
|                              |                                 |
|              +---------------+---------------+                |
|              v               v               v                |
|      +--------------+ +--------------+ +--------------+      |
|      | 增量標記      | | 漸進式根掃描  | | 增量清除      |      |
|      | (5ms budget) | | (5ms budget) | | (5ms budget) |      |
|      +--------------+ +--------------+ +--------------+      |
|                                                               |
+---------------------------------------------------------------+
```

### 2.2 新增模組結構

```
src/gc/
├── incremental/
│   ├── mod.rs              # 模組導出
│   ├── state.rs            # IncrementalMarkState
│   ├── scheduler.rs        # 增量調度器
│   ├── barrier.rs          # 寫入屏障
│   └── slice/
│       ├── marking.rs      # 增量標記切片
│       ├── sweeping.rs     # 增量清除切片
│       └── roots.rs        # 漸進式根掃描
```

---

## 3. 增量狀態管理

### 3.1 IncrementalMarkState 結構

```
IncrementalMarkState 結構包含以下欄位：
- is_incremental: AtomicBool（是否處於增量模式）
- phase: AtomicU8（當前收集階段）
- marked_bytes: AtomicUsize（已標記的位元組數）
- total_bytes_to_mark: AtomicUsize（總共需要標記的位元組數）
- work_units_completed: AtomicUsize（已完成的工作單元數）
- total_work_units: AtomicUsize（總工作單元數）
- slice_budget_ms: AtomicU64（每次切片的時間預算，毫秒）
- last_slice_duration_ms: AtomicU64（最後一次切片的實際耗時）
- collection_start_ns: AtomicU64（增量收集開始時間）
- roots_scanned: AtomicUsize（根掃描進度）
- total_roots: AtomicUsize（總根數）
- pages_swept: AtomicUsize（清除進度）
- total_pages: AtomicUsize（總頁數）
- collection_id: AtomicU64（收集 ID）

常數：
- WORK_UNIT_SIZE: 64 * 1024（64KB 工作單元）
- DEFAULT_SLICE_BUDGET_MS: 5（預設切片時間預算）
- PHASE_IDLE: 0
- PHASE_ROOTS: 1
- PHASE_MARKING: 2
- PHASE_SWEEPING: 3
- PHASE_COMPLETE: 4
```

### 3.2 階段轉換圖

```
                    +----------------+
                    |     IDLE       |<-------------------------------+
                    +-------+--------+                               |
                            | start_incremental_collection()         |
                            v                                       |
                    +----------------+                               |
                    |     ROOTS      |---掃描完成--+                  |
                    +-------+--------+            |                  |
                            |                     |                  |
                            | 5ms budget          |                  |
                            v                     |                  |
                    +----------------+            |                  |
              +----->|   MARKING      |<-         |                  |
              |     +-------+--------+            |                  |
              |             |標記完成              |                  |
              |             v                     |                  |
              |     +----------------+            |                  |
              |     |   SWEEPING     |---清除完成--+                  |
              |     +-------+--------+            |                  |
              |             |清除預算不足          |                  |
              |             v                     |                  |
              |     +----------------+            |                  |
              |     |   YIELD        |------------+                  |
              |     | (回到標記)      |                               |
              |     +-------+--------+                               |
              |             |清除完成                                 |
              +-------------+----------------------------------------+
                    +----------------+
                    |   COMPLETE     |---下一輪 GC ---> IDLE
                    +----------------+
```

### 3.3 原子操作順序約定

| 欄位 | 讀取順序 | 寫入順序 | 理由 |
|------|----------|----------|------|
| is_incremental | Acquire | Release | 輕量級開關 |
| phase | Acquire | AcqRel | 階段轉換需要同步 |
| marked_bytes | Relaxed | Relaxed | 統計用途，最終一致性即可 |
| work_units_completed | Relaxed | Relaxed | 進度追蹤 |
| slice_budget_ms | Acquire | Release | 配置參數 |
| collection_id | Relaxed | Release | 版本標識 |

---

## 4. 增量標記

### 4.1 標記切片執行

```
mark_slice 函數流程：
1. 載入切片時間預算（budget）
2. 記錄開始時間
3. 主要標記迴圈：
   - 優先處理本地工作佇列
   - 嘗試竊取遠程工作（最多 16 次）
   - 檢查時間預算，超過則退出
   - 讓出 CPU 給其他執行緒
4. 記錄切片耗時
5. 返回標記的位元組數

mark_object 函數流程：
1. 獲取物件的頁面和索引
2. 嘗試設置標記位
3. 如果是新標記：
   - 增加標記計數
   - 遞迴標記子物件
4. 返回物件大小
```

### 4.2 標記完整性保證

```
is_marking_complete：檢查工作單元是否完成
finish_marking：Stop-the-World 模式下強制完成標記
```

---

## 5. 寫入屏障

### 5.1 頁面級髒追蹤

rudo-gc 已有 dirty_bitmap，增量 GC 將擴展其用途：

```
PageHeader 新增方法：
- is_dirty(index): 檢查物件是否為髒
- set_dirty(index): 設置物件為髒
- clear_all_dirty(): 清除所有髒位
```

### 5.2 寫入屏障實現

```
WriteBarrier::record_write 函數：
1. 檢查是否在增量收集期間
2. 檢查來源是否在堆積中
3. 設置來源物件的髒位
4. 記錄跨代引用（未來擴展）
```

### 5.3 屏障集成點

```
GcCell::set_internal 集成寫入屏障：
- 在寫入前記錄舊值
- 執行寫入
- 在寫入後記錄新值
```

---

## 6. 漸進式根掃描

### 6.1 根掃描切片

```
IncrementalRootScanner 結構：
- state: 狀態共享指標
- collected_roots: 收集的根引用
- current_thread_index: 當前執行緒索引
- total_threads: 總執行緒數
- thread_scan_progress: 執行緒掃描進度

scan_thread_slice 函數：
1. 獲取執行緒棧邊界
2. 計算本次可掃描大小
3. 執行保守式掃描
4. 更新進度
5. 讓出 CPU（如果超時）
```

---

## 7. 增量清除

### 7.1 兩階段清除設計

```
PendingDrop 結構：
- page: 物件所在頁面
- index: 頁面中的索引
- ptr: 物件指標
- size: 物件大小

IncrementalSweeper 結構：
- state: 狀態共享指標
- pending_finalize: 待解構任務
- pending_reclaim: 待回收任務
- current_page: 當前頁面

兩階段流程：
第一階段：執行解構函數
  - 有弱引用：只清除值
  - 無弱引用：完全解構，加入待回收列表

第二階段：回收記憶體
  - 清除分配位和標記位
  - 將槽位加入釋放清單
```

---

## 8. 增量調度器

### 8.1 調度策略

```
IncrementalScheduler 結構：
- state: 全局狀態
- sweeper: 清除器
- root_scanner: 根掃描器
- worklists: 工作清單
- steal_queue: 竊取佇列

run_collection 主流程：
1. 初始化狀態
2. 漸進式根掃描
3. 增量標記
4. 增量清除
5. 完成收集
```

---

## 9. API 擴展

### 9.1 新增配置選項

```
IncrementalConfig 結構：
- enabled: 是否啟用增量收集
- slice_budget_ms: 切片時間預算
- max_incremental_heap_bytes: 最大堆積大小
- max_incremental_work_units: 最大工作單元數
```

### 9.2 指標 API

```
Gc<T> 新增方法：
- is_marked(): 物件是否被標記
- is_dirty(): 物件頁面是否為髒
```

---

## 10. 效能特性

### 10.1 暫停時間保證

| 操作 | 最壞情況 |
|------|----------|
| 安全點檢查 | < 0.1ms |
| 寫入屏障 | < 10ns |
| 標記切片 | <= 5ms |
| 清除切片 | <= 5ms |
| 根掃描切片 | <= 5ms |

### 10.2 記憶體開銷

| 元件 | 開銷 |
|------|------|
| IncrementalMarkState | ~200 位元組 |
| IncrementalSweeper | ~100 位元組 |
| 寫入屏障 | 每物件寫入 +1 原子操作 |

---

## 11. 測試策略

### 11.1 測試類型

- 單元測試：狀態轉換、切片執行
- 整合測試：無記憶體洩漏、最大暫停時間
- 並列測試：多執行緒同時分配
- Miri 測試：記憶體安全驗證

---

## 12. 實作檢查清單

### Phase 1：增量基礎設施
- [ ] 新增 src/gc/incremental/mod.rs 模組
- [ ] 實現 IncrementalMarkState 結構
- [ ] 修改 collect() 入口支援增量模式
- [ ] 新增單元測試

### Phase 2：寫入屏障
- [ ] 擴展 PageHeader::dirty_bitmap 介面
- [ ] 實現 WriteBarrier::record_write()
- [ ] 在 GcCell::set_internal() 集成屏障
- [ ] 新增整合測試

### Phase 3：漸進式根掃描
- [ ] 實現 IncrementalRootScanner 結構
- [ ] 修改 conservative_scan() 支援切片
- [ ] 新增整合測試

### Phase 4：增量清除
- [ ] 實現 IncrementalSweeper 結構
- [ ] 實現 prepare_sweep() 識別待清除物件
- [ ] 實現 sweep_slice() 兩階段清除
- [ ] 新增整合測試

### Phase 5：調度與完成
- [ ] 實現 IncrementalScheduler
- [ ] 實現 Stop-the-World 回退機制
- [ ] 新增端到端測試

### Phase 6：API 與配置
- [ ] 新增 IncrementalConfig 結構
- [ ] 擴展 GcConfig API
- [ ] 更新文件
- [ ] 執行完整測試套件

---

## 13. 參考文獻

1. McCarthy, J. (1960). "Recursive Functions of Symbolic Expressions"
2. Appel, A. W. (1987). "Garbage Collection Can Be Faster Than Allocation"
3. Hudson, R. L., et al. (1991). "Incremental Garbage Collection for Generational Collectors"
4. Dybvig, R. K. (2006). "Chez Scheme Version 8 User's Guide"
5. Jones, R., et al. (2012). "The Garbage Collection Handbook"

---

## 14. 總結

本規格文件定義了 rudo-gc 增量收集器的完整實現方案。

設計原則：
1. 正確性優先：使用保守式標記、兩階段清除
2. 漸進式實作：分六個 Phase 交付
3. 效能可控：5ms 切片預算可配置
4. 向後相容：現有 API 完全保持不變

---

文件版本：v1.0
日期：2026-01-29
