# Parallel Marking 技術規格：基於 Message Passing (Remote Mentions)

## 1. 設計目標
為 `rudo-gc` 實作高效的並行標記階段，利用 BiBOP (Big Bag of Pages) 的幾何特性，在多核心環境下最大化吞吐量並最小化快取爭用 (Cache Contention)。

### 為什麼選擇 Message Passing 而非 Work-stealing？
1.  **無鎖標記 (Lock-free Fast-path)**：利用 BiBOP 頁面所有權，執行緒在標記本地物件時無需使用 `atomic_or` 指令。
2.  **快取親和性 (Cache Locality)**：每個執行緒只會修改自己擁有的 Mark Bitmap，避免 CPU 核心間的 False Sharing。
3.  **BiBOP 優勢**：透過一次 `AND` 運算即可判斷指標歸屬，轉發開銷極低。

---

## 2. 數據結構變更

### A. PageHeader (堆頁面頭部)
必須明確標記頁面所有者。
```rust
struct PageHeader {
    // ... 現有欄位
    pub owner_id: usize, // 擁有此頁面的執行緒 ID (對應 TCB 索引)
}
```

### B. ThreadControlBlock (執行緒控制塊)
增加標記任務隊列與交換信箱。
```rust
pub struct ThreadControlBlock {
    // ... 現有欄位
    /// 本地待掃描物件棧 (執行緒私有)
    pub local_mark_stack: UnsafeCell<Vec<*const GcBox<()>>>,
    /// 來自其他執行緒的轉發物件信箱 (鎖保護或 Lock-free)
    pub remote_inbox: Mutex<Vec<*const GcBox<()>>>,
}
```

---

## 3. 並行標記演算法

### 核心 Worker 邏輯 (STW 期間執行)
每個參與 GC 的執行緒將運行以下循環：

1.  **本地標記階段**：
    -   從 `local_mark_stack` 彈出物件。
    -   調用 `trace_fn` 掃描子指標。
    -   對於每個子指標 `ptr`：
        -   計算 `owner_id = ptr_to_page_header(ptr).owner_id`。
        -   如果 `owner_id == self.id`：
            -   **本地物件**：檢查 Mark Bitmap (普通 `u64` 操作)。若未標記，則標記並推入 `local_mark_stack`。
        -   否則：
            -   **遠端物件**：將指標轉發至 `registry.threads[owner_id].remote_inbox`。

2.  **訊息交換階段**：
    -   若 `local_mark_stack` 為空，嘗試從自己的 `remote_inbox` 獲取一批物件。
    -   將獲取的物件存入 `local_mark_stack` 並回到步驟 1。

3.  **終止檢測 (Termination Detection)**：
    -   使用原子計數器 `active_workers` 或屏障 (Barrier) 機制。
    -   當 `local_mark_stack` 與 `remote_inbox` 皆為空，且全域無在途訊息時，標記完成。

---

## 4. 關鍵優化：內部指標處理
在並行標記中，`find_gc_box_from_ptr` 必須維持 O(1) 效能。
-   **本地檢查**：執行緒優先檢查 `self.heap.small_pages`。
-   **遠端檢查**：若指標不在本地範圍，轉發至擁有者進行最終校驗。這能有效分擔「偽指標 (False Positives)」的校驗開銷。

---

## 5. 實作階段規劃

### 第一階段：所有權與隊列 (Phase 1)
-   在 `LocalHeap` 分配頁面時自動寫入 `owner_id`。
-   在 `ThreadControlBlock` 實作 `remote_inbox` 與 `local_mark_stack`。

### 第二階段：GcVisitor 轉發邏輯 (Phase 2)
-   重構 `GcVisitor`：
    -   `visit` 函式現在需要根據所有權決定「直接標記」或「轉發訊息」。
    -   支援「重新掃描 (Re-sweep)」邏輯以處理接收到的遠端物件。

### 第三階段：並行協調器與終止檢測 (Phase 3)
-   實作 `perform_parallel_marking` 函式。
-   建立執行緒間的工作交換機制。
-   完善終止檢測邏輯，防止執行緒過早退出。

---

## 6. McCarthy 的架構備忘錄
> 「記住，並行並非單純地將工作拆分，而是要確保工作之間的邊界清晰。BiBOP 就是這道邊界。我們不應在不同 CPU 核心之間搬運鎖，而應搬運數據。讓每個核心專注於它所分配的那塊記憶體，這是通往極致效能的唯一途徑。」
