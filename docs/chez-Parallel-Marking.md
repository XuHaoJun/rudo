# Chez Scheme Parallel Marking 實作分析與 rudo-gc 移植建議

## 1. Chez Scheme Parallel Marking 核心機制

基於對 `c/gc.c` 與 `c/gc-par.c` 的原始碼分析，Chez Scheme 的並行 GC（包含 Marking 與 Sweeping）採用了一套優雅的 **「所有權導向 (Ownership-based)」** 並行策略，有效解決了保守式 GC 在多執行緒環境下的競爭問題。

### A. 執行緒與 Sweeper 模型
- **Sweeper Threads**：GC 啟動時會分配多個 Sweeper 執行緒（對應 `maximum_parallel_collect_threads`）。
- **所有權 (Ownership)**：每個 BiBOP Segment (Page) 在 `seginfo` 中都有一個 `creator` 欄位，指向擁有該頁面的 `thread_gc`。
- **本地化處理**：一個 Sweeper 只負責處理它所分配到的 `thread_gc` 所擁有的頁面。這意味著在標記 (Marking) 時，對 Mark Bitmap 的操作是單執行緒的，完全不需要鎖或原子操作。

### B. 遠端引用 (Remote Mentions) - 並行追蹤的核心
這是 Chez 最具精華的部分。當 Sweeper A 在掃描物件時，發現一個指向 Sweeper B 所擁有頁面的指標：
1. **不直接追蹤**：Sweeper A 不會去修改 Sweeper B 頁面的標記位。
2. **遠端轉發**：Sweeper A 將「包含該指標的物件」放入一個 **`send_remote_sweep_stack`**，並標記目標為 Sweeper B。
3. **交換機制**：透過 `send_and_receive_remote_sweeps` 函式，Sweeper A 將任務交給 Sweeper B。
4. **重新掃描**：Sweeper B 接收到物件後，在其本地環境下對該物件進行重新掃描 (Re-sweep)，從而正確標記其擁有的物件。

### C. 工作協調與終止 (Coordination & Termination)
- **工作狀態**：Sweeper 有四種狀態：`NONE`, `READY`, `SWEEPING`, `WAITING_FOR_WORK`。
- **終止條件**：只有當所有 Sweeper 都處於 `WAITING_FOR_WORK` 且所有遠端轉發隊列 (Remote Stacks) 都為空時，標記階段才算完成。

---

## 2. 移植至 rudo-gc 的「精華部分」

為了讓 `rudo-gc` 支援並行標記，我們應借鑑 Chez 的「遠端引用」機制，並結合 Rust 的安全性：

### 核心設計：基於 Message Passing 的並行標記

#### 1. 頁面所有權標記
在 `PageHeader` 中加入 `owner_id` (對應 `ThreadId`)：
```rust
struct PageHeader {
    owner_id: ThreadId, // 標記哪個線程擁有此頁面
    // ...
}
```

#### 2. 執行緒本地標記隊列 (Marking Queues)
每個 `ThreadControlBlock` 增加兩個隊列：
- **`local_mark_stack`**: 存放本線程擁有的待掃描物件。
- **`remote_inbox`**: `Mutex<Vec<*const GcBox<()>>>`，接收來自其他線程的轉發物件。

#### 3. 並行標記演算法 (Worker Logic)
每個線程在 STW 期間執行：
```rust
fn marking_worker(tcb: &ThreadControlBlock) {
    loop {
        // 1. 處理本地隊列
        while let Some(obj) = tcb.local_mark_stack.pop() {
            scan_object(obj, |ptr| {
                let header = ptr_to_page_header(ptr);
                if header.owner_id == tcb.id {
                    // 本地物件：直接標記並入隊
                    if !header.is_marked(ptr) {
                        header.set_mark(ptr);
                        tcb.local_mark_stack.push(ptr);
                    }
                } else {
                    // 遠端物件：轉發給擁有者 (Remote Mention)
                    forward_to_owner(header.owner_id, obj);
                }
            });
        }

        // 2. 檢查 Inbox 是否有遠端轉發過來的任務
        if !tcb.fetch_remote_work() {
            // 3. 如果連 Inbox 都空了，進入等待或終止檢查
            if all_workers_waiting() { break; }
        }
    }
}
```

### 為什麼這對 rudo-gc 最合適？
1. **無鎖 Fast-path**：標記本線程物件時不需要任何原子操作或鎖，效能極高。
2. **避免 Cache 爭用**：不同線程操作不同的 Mark Bitmaps，不會產生 CPU Cache Line 的 False Sharing。
3. **適配 BiBOP**：BiBOP 的頁面對齊讓「判斷所有權」只需一次位元運算，成本極低。

## 3. 待辦事項 (建議實作順序)
1. 在 `ThreadControlBlock` 中建立任務交換信箱。
2. 修改 `GcVisitor` 使其能識別跨執行緒指標並進行轉發。
3. 實作終止檢測器 (Termination Detector)，確保所有執行緒都完成工作。
