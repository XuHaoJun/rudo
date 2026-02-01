# Chat Conversation

Note: _This is purely the output of the chat conversation and does not contain any raw data, codebase snippets, etc. used to generate the output._

### User Input

read@[README.md] , as rust  leadership-council, see this crate 有什麼感想

*Viewed [README.md](file:///home/noah/Desktop/rudo/README.md) *

### Planner Response

這是一份來自 **Rust Leadership Council** 視角（模擬）的 Code Review 與回饋。作為 Rust 專案的領導層，我們高度重視 **安全性 (Safety)**、**健全性 (Soundness)**、**人體工學 (Ergonomics)** 以及 **與 Rust 生態系的整合 (Ecosystem coherence)**。

閱讀完 `rudo-gc` 的 README 後，以下是我們對於這個 crate 的感想：

---

### 🦀 Rust Leadership Council Memo: `rudo-gc` Review

**Verdict:** 充滿野心且架構有趣的專案，但在 **安全性邊界 (Safety Boundaries)** 與 **Async 整合** 上有幾個需要高度警戒的設計點。

#### 1. 關於 "Conservative Stack Scanning" (保守式堆疊掃描)
*   **優點**: 對使用者來說，這極大地降低了入門門檻（不需要傳遞 Context 或 Lifetime），這點很像 V8 或 Go 的體驗。
*   **Council 的擔憂**: 在 Rust 中做 Conservative Stack Scanning 是非常危險的。Rust 編譯器 (基於 LLVM) 並不知道 GC 的存在，優化器可能會：
    *   **Spill registers**: 將指標暫存到我們掃描不到的地方（雖然通常會在 stack 上）。
    *   **Pointer Provenance**: Rust 正致力於更嚴格的 Strict Provenance 模型。保守掃描通常只看數值 (usize)，這在未來的 Rust 版本或 Miri檢查中可能會遇到 "ptr to int" 轉換的未定義行為 (UB) 邊緣案例。
    *   **False Negatives (漏掃)**: 雖然 README 提到 "False Positives" (記憶體洩漏/bloat) 是可接受的代價，但 **False Negatives** (掃描器沒看到指標導致 UAF) 才是致命傷。Non-moving GC 確實規避了更新指標的問題，但 "掃瞄不到" 的風險依然存在於高度優化的代碼中。

#### 2. 關於 Tokio/Async 整合的 `root_guard()`
這是此設計中最脆弱的一環：
> "When accessing `Gc<T>` inside `tokio::spawn`, you must register it as a root using `root_guard()`"

*   **安全性漏洞**: 這把記憶體安全的責任從 **編譯器** 轉移到了 **開發者的人腦**。如果使用者在 `tokio::spawn` 裡忘了寫 `let _guard = gc.root_guard();`，而這個 task 後來被暫停 (yield)，發生 GC 時，這個 `Gc<T>` 就可能被回收，接著 task 醒來後存取 -> **Use-After-Free (UAF)**。
*   **Unsound API**: 在 Rust 的哲學裡，能夠導致 UAF 的操作必須被標記為 `unsafe`，或者 API 設計上要讓它無法編譯。目前的設計似乎允許在 Safe Rust 中犯下這個錯誤，這在我們看來是 Unsound 的。
    *   *建議*: 考慮是否能透過類似 `spawn_with_gc` 的 wrapper 來強制 root 註冊，或者利用 Type System 強制要求 `Send` 到 task 的 GC 必須被 wrap。

#### 3. 關於 `Sync` 與 `Send`
> "`Gc<T>` implements `Send` and `Sync` when `T: Send + Sync`"

*   這在具有 Thread-Local Allocation Buffer (TLAB) 的 GC 中是一個強大的承諾。
*   **Safepoint 機制**: README 提到需要 "Cooperative rendezvous protocol"。如果一個執行緒卡在第三方 C library 或者長時間的迴圈中忘了呼叫 `safepoint()`，它會阻擋全世界 (Stop-The-World) 嗎？這對於即時性要求高的 Rust 應用（如遊戲或高頻交易）是個潛在痛點。

#### 4. 架構亮點 (The Good Parts)
*   **BiBOP + Non-moving**: 這是一個非常務實的選擇。在 Rust 中實作 Moving GC 極其困難（因為無法輕易更新 stack 上的 reference），採用 Non-moving 策略讓 `&T` 可以穩定存在，這是非常 Rusty 的正確決定。
*   **Layering**: 分離 `lazy-sweep`, `tokio` features 是良好的工程實踐。
*   **Miri Support**: 如果你們真的跑通了 Miri，那代表底層的 unsafe 指標操作有相當程度的嚴謹性，這一點值得高度讚賞。

### 總結

`rudo-gc` 試圖解決 Rust 生態系中 "具有循環引用的複雜圖結構" 這個痛點，並且選擇了一條艱難但高效的路（保守掃描 + 並行標記）。

**給團隊的建議 (Actionable Feedback):**
1.  **審視 Tokio API 安全性**: 這是目前最明顯的 unsoundness 來源。請考慮如何讓 "忘記 root" 變成編譯錯誤，而不是執行期 UAF。
2.  **文件警示**: 對於 Stack Scanning 的限制需要更誠實的警示（例如：某些架構或編譯器優化等級下的行為）。
3.  **Stress Tests**: 針對 false negatives 做大量的 fuzzing 測試，特別是在高優化等級 (`release` mode) 下。

這個專案看起來像是把 Scheme 的靈魂裝進了 Rust 的身體，非常有意思，但請務必小心 `unsafe` 的邊界。

— *The Rust Leadership Council (Simulated)*

### User Input

調查 @[learn-projects/dumpster] @[learn-projects/gc-arena] 是怎麼解決 Conservative Stack Scanning

*Listed directory [dumpster](file:///home/noah/Desktop/rudo/learn-projects/dumpster) *

*Listed directory [gc-arena](file:///home/noah/Desktop/rudo/learn-projects/gc-arena) *

*Listed directory [src](file:///home/noah/Desktop/rudo/learn-projects/dumpster/dumpster/src) *

*Listed directory [src](file:///home/noah/Desktop/rudo/learn-projects/gc-arena/src) *

*Viewed [README.md](file:///home/noah/Desktop/rudo/learn-projects/dumpster/README.md) *

*Viewed [README.md](file:///home/noah/Desktop/rudo/learn-projects/gc-arena/README.md) *

*Listed directory [unsync](file:///home/noah/Desktop/rudo/learn-projects/dumpster/dumpster/src/unsync) *

*Read URL content from /home/noah/Desktop/rudo/learn-projects/gc-arena/src/arena.rs *

*Viewed [arena.rs](file:///home/noah/Desktop/rudo/learn-projects/gc-arena/src/arena.rs) *

*Viewed [mod.rs](file:///home/noah/Desktop/rudo/learn-projects/dumpster/dumpster/src/unsync/mod.rs) *

### Planner Response

根據調查結果，這兩個專案其實並沒有「解決」被動式堆疊掃描 (Conservative Stack Scanning) 的技術難題（例如如何精準識別 pointer），而是透過完全不同的記憶體管理策略來 **徹底避免 (Avoid)** 使用堆疊掃描。

以下是 `dumpster` 與 `gc-arena` 的詳細機制分析與比較：

### 1. `dumpster`
**策略：參照計數 (Reference Counting / RAII) + 循環偵測**

*   **運作原理**：
    *   `dumpster` 的核心是一個增強版的 `Rc<T>`。所有的 `Gc<T>` 指標其實就是帶著 Reference Count 的智慧指標。
    *   **不需要掃描 Stack**：因為它依賴 Rust 的 `Drop` 機制。當一個 `Gc<T>` 變數存在於 stack 上時，它的 ref count 至少為 1 (Strong Reference)。當函數返回或變數離開作用域，`Drop` 會自動被呼叫，ref count 減 1。
    *   **Root 的定義**：Stack 上的變數透過 RAII 自動管理自己的存活，不需要 GC 介入去「找」它們。只有當 Heap 上的物件形成孤島循環 (Cycle) 且外部 Ref Count 為 0 時，才需要 GC 介入。
    *   **循環偵測**：它會在一個 thread-local 的 `Dumpster` 結構中記錄物件的創建與銷毀，並定期執行類似 tracing 的演算法來清理那些 Ref Count 不為 0 但實際上不可達的循環引用。

*   **結論**：它將 Stack Root 的管理責任交給了 Rust 的編譯器 (RAII)，GC 只需要專注於 Heap 上的循環回收。

### 2. `gc-arena`
**策略：生成式生命週期 (Generativity / Branding) + 區域化回收**

*   **運作原理**：
    *   這是一個**非常 Rust** 的解法。它利用 Rust 的生命週期 (Lifetime) 系統來強行限制 GC 指標的存活範圍。
    *   **不需要掃描 Stack**：因為它透過編譯器保證在 GC 發生的當下，Stack 上**絕對不會有未被追蹤的 GC 指標**。
    *   **關鍵機制 (`Mutator` + `Branding`)**：
        *   使用者必須透過一個 callback (例如 `arena.mutate(...)`) 來存取 GC 物件。
        *   這個 callback 會獲得一個帶有獨特生命週期 `'gc` 的 Context。
        *   在此 callback 內產生的所有 `Gc<'gc, T>` 指標都被打上了這個 `'gc` 烙印。
        *   **Rust Borrow Checker 的保證**：由於 `'gc` 是不變變體 (invariant) 且只存在於該 callback 內，這些指標**無法**被傳遞到 callback 外部，也無法在 callback 結束後存活。
    *   **回收時機**：當 callback 結束，Stack 乾乾淨淨，所有的指標都回到了 Arena 的管理下（或者死了）。這時執行 `arena.collect()`，GC 只需要從 Arena 內部保存的單一 Root (Root Object) 開始 Trace 即可，完全不必看 Stack。

*   **結論**：它透過 Type System 證明了 Stack 在回收當下是乾淨的，直接消滅了掃描 Stack 的需求。

### 對比 `rudo-gc` (目前的設計)

| 特性 | **rudo-gc** (你目前的專案) | **dumpster** | **gc-arena** |
| :--- | :--- | :--- | :--- |
| **Stack Root 來源** | **保守掃描 (Conservative Scan)**：暴力掃描 stack memory，猜測哪些是指標。 | **RAII (Drop)**：依賴變數離開作用域自動解引用。 | **無 (None)**：編譯器保證回收時 Stack 上無指標。 |
| **優點** | **使用體驗極佳**：寫起來跟普通 Rust code 幾乎一樣，不需要特殊的 wrapper 或 context。 | **自動化**：類似 `Rc`，但能處理循環。 | **極致安全 (Soundness)**：不可能發生 UAF，完全符合 Rust 安全模型。 |
| **缺點** | **安全性/精確性疑慮**：可能發生 False Positive (Leak) 或 False Negative (UAF)，且與優化器對抗。 | **效能開銷**：每次 clone/drop 都有 atomic/cell 操作；循環偵測有額外開銷。 | **使用門檻高**：必須在 [mutate](cci:1://file:///home/noah/Desktop/rudo/learn-projects/gc-arena/src/arena.rs:208:4-222:5) closure 內寫 code，無法隨意傳遞指標，且結構需支援 [Trace](cci:2://file:///home/noah/Desktop/rudo/learn-projects/dumpster/dumpster/src/unsync/mod.rs:58:0-61:1)。 |

**給您的建議：**
既然您收到 Rust Leadership Council 的回饋是擔憂 Stack Scanning 的安全性，而您如果不想犧牲 `rudo-gc` 的易用性（改成 `gc-arena` 那種寫法會大幅改變體驗），您可能需要：
1.  **堅持路線但加強防護**：參考 `Boehm GC` 的作法，盡量使用 `volatile` 或 compiler fence 來防止優化器隱藏指標（雖然在 Rust 中這很難做且不保證）。
2.  **混合策略**：保留 Stack Scanning 用於快速原型，但提供一個 `Root<T>` 的 wrapper (類似 Handle)，鼓勵使用者顯式註冊 Root 來換取 100% 的安全性，就像 V8 的 `HandleScope`。

Step Id: 29
<EPHEMERAL_MESSAGE>
Currently you have open:
- 1 markdown document(s)
- 3 rust document(s)
- 1 toml document(s)

You have permissions to:
- write to files
- read local files
- run commands
- use a browser

You can close open files to declutter your workspace if they are not needed.
</EPHEMERAL_MESSAGE>

### User Input

1. 關於 "Conservative Stack Scanning" (保守式堆疊掃描)
優點: 對使用者來說，這極大地降低了入門門檻（不需要傳遞 Context 或 Lifetime），這點很像 V8 或 Go 的體驗。
Council 的擔憂: 在 Rust 中做 Conservative Stack Scanning 是非常危險的。Rust 編譯器 (基於 LLVM) 並不知道 GC 的存在，優化器可能會：
Spill registers: 將指標暫存到我們掃描不到的地方（雖然通常會在 stack 上）。
Pointer Provenance: Rust 正致力於更嚴格的 Strict Provenance 模型。保守掃描通常只看數值 (usize)，這在未來的 Rust 版本或 Miri檢查中可能會遇到 "ptr to int" 轉換的未定義行為 (UB) 邊緣案例。
False Negatives (漏掃): 雖然 README 提到 "False Positives" (記憶體洩漏/bloat) 是可接受的代價，但 False Negatives (掃描器沒看到指標導致 UAF) 才是致命傷。Non-moving GC 確實規避了更新指標的問題，但 "掃瞄不到" 的風險依然存在於高度優化的代碼中。


請你深度閱讀@[crates/rudo-gc] 後，再回答該疑慮

*Listed directory [src](file:///home/noah/Desktop/rudo/crates/rudo-gc/src) *

*Viewed [stack.rs](file:///home/noah/Desktop/rudo/crates/rudo-gc/src/stack.rs) *

*Viewed [scan.rs](file:///home/noah/Desktop/rudo/crates/rudo-gc/src/scan.rs) *

*Viewed [lib.rs](file:///home/noah/Desktop/rudo/crates/rudo-gc/src/lib.rs) *

*Viewed [heap.rs](file:///home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs) *

*Viewed [heap.rs](file:///home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs) *

*Grep searched codebase*

*Viewed [heap.rs](file:///home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs) *

*Viewed [blacklisting_test.rs](file:///home/noah/Desktop/rudo/crates/rudo-gc/src/blacklisting_test.rs) *

### Planner Response

該 `rudo-gc` 專案在實作上有不少亮點（如 BiBOP 架構、Miri 支援的嘗試），但經過深度審閱程式碼後，必須嚴肅地告訴您：**Rust Leadership Council 的擔憂是完全正確且命中的，目前的實作存在嚴重的 Soundness 漏洞。**

以下是針對 **Conservative Stack Scanning** 的深度分析報告：

### 1. 致命傷：False Negatives (漏掃) 與 Use-After-Free
這不是「理論上的風險」，而是程式碼中實實在在的邏輯漏洞。

*   **問題所在的程式碼**: [crates/rudo-gc/src/heap.rs](cci:7://file:///home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs:0:0-0:0) 中的 [find_gc_box_from_ptr](cci:1://file:///home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs:2152:0-2283:1) 函數 (Lines 2266-2272)。
*   **發現**:
    ```rust
    } else if offset_to_use % block_size_to_use != 0 {
        // For small objects, we still require them to point to the start of an object
        return None; 
    }
    ```
*   **後果**: 對於 Small Objects（小於 2KB），只要 Stack 上只剩下指向該物件**內部欄位**的指標（Interior Pointer），GC 就會認為這是「無效指標」並回收該物件。
*   **重現場景 (SROA)**:
    ```rust
    struct Node { a: u64, b: u64 } // Size: 16 bytes
    let node = Gc::new(Node { a: 1, b: 2 });
    let ref_b = &node.b; // 指向 offset + 8 的位置
    drop(node); // stack 上只剩 ref_b
    
    // GC 發生：
    // Scanner 掃到 ref_b (interior pointer)
    // find_gc_box 計算 offset = 8
    // block_size = 16
    // 8 % 16 != 0 -> return None (認為不是 GC 指標)
    // 結果：Node 被回收，ref_b 變成懸空指標 (Dangling Pointer) -> UAF!
    ```
    這在 Rust 高度優化 (Release Mode) 下非常容易發生，因為編譯器喜歡對 Struct 做 Scalar Replacement of Aggregates (SROA)，把結構拆散處理。這對於號稱 Safety 的 GC 來說是致命的。

### 2. 優化器與暫存器的戰爭 (Register Spilling)
*   **實作**: [crates/rudo-gc/src/stack.rs](cci:7://file:///home/noah/Desktop/rudo/crates/rudo-gc/src/stack.rs:0:0-0:0) 裡的 [spill_registers_and_scan](cci:1://file:///home/noah/Desktop/rudo/crates/rudo-gc/src/stack.rs:130:0-229:1)。
*   **現況**: 作者手動列出了 x86_64 和 aarch64 的 Callee-Saved 暫存器並用 `asm!` spill 到 stack 上。
*   **風險**: 這是一個與編譯器「軍備競賽」的過程。雖然目前抓住了 Callee-Saved regs，但如果 LLVM 決定把指標暫存在 **Caller-Saved** 暫存器（例如在迴圈優化中）或者使用了 Vector Registers (AVX/SSE) 來搬運數據，這些都不在掃描範圍內。
*   **黑魔法**: 使用 `black_box` 是一個正確的防禦，但它無法保證覆蓋所有優化路徑。

### 3. Pointer Provenance (指標系譜)
*   **實作**: 整個 [scan.rs](cci:7://file:///home/noah/Desktop/rudo/crates/rudo-gc/src/scan.rs:0:0-0:0) 和 [heap.rs](cci:7://file:///home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs:0:0-0:0) 充斥著 `ptr as usize` 和 `usize as ptr` 的轉換。
*   **風險**:
    *   在 **Strict Provenance** 模型下，這是 Unsound 的。把指標轉成整數 (Expose) 再轉回來 (FromExposed)，在目前的硬體上「通常」能跑，但在未來的 Rust 優化甚至硬體（如 CHERI 架構）上會直接崩潰。
    *   **Miri Hack**: 作者在 [find_gc_box_from_ptr](cci:1://file:///home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs:2152:0-2283:1) 裡寫了一大段 `#[cfg(miri)]` 來「作弊」——當 Miri 執行時，它不真的用算出來的指標，而去遍歷 heap 找回原本的指標來恢復 Provenance。這證明了作者知道目前的指針運算是不合法的，只是為了讓測試通過而繞過檢查。

### 結論與建議

**審查結果**: 目前的 `Conservative Stack Scanning` 實作是 **Unsound** 的，尤其是不支援 Small Objects 的 Interior Pointers 這一點，會導致 Release Build 在無預警下發生記憶體崩潰。

**給開發者的建議 (Action Items)**:
1.  **Immediate Fix (High Priority)**: 在 [find_gc_box_from_ptr](cci:1://file:///home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs:2152:0-2283:1) 中，針對 Small Objects **必須支援 Interior Pointers**。您需要計算 `index = offset / block_size` 後，容許 `offset % block_size != 0`，只要確認 [index](cci:1://file:///home/noah/Desktop/rudo/crates/rudo-gc/src/heap.rs:926:0-945:1) 在合法範圍且該 slot 是對應的 allocated object 即可。
2.  **長期策略**: 如果要追求極致效能與 Rust 原生整合，建議放棄 Conservative Stack Scanning，轉向 `gc-arena` 的 **Generativity (Branding)** 路線，或是像 `dumpster` 那樣依賴 **RAII (Drop)** 來管理 Root。如果堅持要用 Stack Scanning，必須接受這是一個永遠充滿 `unsafe` 與 UB 邊緣試探的戰場。

這不只是一個「功能建議」，更是關乎這個 `Gc<T>` 能否在實際生產環境中不造成 Segmentation Fault 的關鍵。

### User Input

我不熟 gc-arena  或  dumpster , 假設使用其中一個的方式, 那為什麼還需要 rudo-gc?

### Planner Response

這是一個非常好的靈魂拷問。

如果您決定採用 `gc-arena` (生命週期監管) 或 `dumpster` (RAII/RefCounting) 的 **Rooting 策略** 來解決安全性問題，**`rudo-gc` 仍然擁有巨大的價值**。

原因在於：**Rooting 策略（怎麼找活著的物件）只是 GC 的一部分，這兩個專案在「記憶體佈局」與「回收效能」上，都沒有 `rudo-gc` 強大。**

以下是為什麼即便換了 Rooting 策略，您還是需要 `rudo-gc` 的三個核心理由：

### 1. 記憶體分配效能 (Allocation Performance)
這是 `rudo-gc` 最強的護城河。

*   **Dumpster / 標準 Rc**: 它們底層通常直接呼叫 `malloc` (Rust 的 Global Allocator)。這意味著每次 `Gc::new` 都要去系統拿記憶體，速度較慢，且記憶體碎片化嚴重。
*   **Rudo-GC**: 您實作了 **BiBOP (Big Bag of Pages)** 和 **TLAB (Thread-Local Allocation Buffer)**。
    *   **Bump Allocation**: `rudo-gc` 分配物件只需要移動一個指標 (pointer bump)，這是 **O(1)** 操作，比 `malloc` 快非常多。
    *   **Cache Locality**: 相關大小的物件都在同一個 Page 裡，這對 CPU Cache 非常友善。

> **結論**：就算您改用了 `dumpster` 的「自動 Drop」策略，如果底層把 `malloc` 換成您的 `BiBOP Allocator`，效能會瞬間起死回生。這就是 `rudo-gc` 存在的意義。

### 2. 並行與擴展性 (Concurrency & Scalability)
*   **Dumpster (Ref Counting)**: 雖然有 [sync](cci:1://file:///home/noah/Desktop/rudo/learn-projects/dumpster/dumpster/src/unsync/mod.rs:604:4-608:5) 版本，但 Reference Counting 在多執行緒下有巨大的 **Cache Contention (快取競爭)** 問題。每次 clone/drop 都要 atomic modify 同一個計數器，這會讓多核心 CPU 互相鎖死。
*   **Gc-Arena**: 目前主要是單執行緒設計 (Not `Send`/`Sync`)，要跨執行緒非常痛苦。
*   **Rudo-GC**: 您設計了 **Stop-The-World (STW)** 和 **Parallel Marking (Work Stealing)**。
    *   在大量物件且頻繁讀寫的場景（例如建構大型 Graph），Tracing GC (標記-清除) 的總吞吐量通常優於 Reference Counting，因為它不需要在每次讀寫時付出代價，只在記憶體不足時一次清算。

> **結論**：對於需要「高效能、多執行緒共用大量物件」的場景（這正是 Rust 的強項），`rudo-gc` 的架構比 `ref count` 更具擴展性。

### 3. 使用者體驗 (Ergonomics) vs. Gc-Arena
*   **Gc-Arena**: 雖然安全，但寫起來極其痛苦。所有的物件操作都被鎖在 `arena.mutate(|mc, root| ...)` 的閉包裡。您無法輕易地把一個 `Gc<T>` 傳給另一個函數，除非該函數也接受那個複雜的 lifetime `'gc`。
*   **Rudo-GC**: 您的目標是讓 `Gc<T>` 用起來像 `Arc<T>` 一樣簡單。

> **結論**：如果您能保留 `rudo-gc` 的底層引擎，但開發出一種「比 gc-arena 簡單，但比 Conservative Scanning 安全」的中間路線（例如 **Explicit Handle Scope**，類似 V8 的 `HandleScope`），那您將統治這塊領域。

---

### 戰略建議：Rudo-GC 的下一步

不要拋棄 `rudo-gc`，而是**升級它的 Rooting 機制**。您可以考慮「混合體」路線：

**保留強大的引擎 (Keep the Engine):**
*   保留 `BiBOP Heap`。
*   保留 `TLAB` 分配器。
*   保留 `Parallel Marker`。

**替換脆弱的掃描器 (Swap the Scanner):**
既然 Stack Scanning 被認為 unsafe，您可以提供兩種模式：
1.  **`unsafe` 模式 (現狀)**: 為了極致開發速度，保留目前的保守掃描，但修好那個 interior pointer 的 bug。
2.  **`Handle` 模式 (推薦)**: 學習 **V8 JavaScript Engine** 的做法。
    *   不強制用閉包 (不像 `gc-arena`)。
    *   但在 Stack 上持有物件時，必須宣告一個 `HandleScope`。
    *   `Gc<T>` 在 Stack 上只是一個 index，真正的指標由 `HandleScope` 管理（這樣 GC 就明確知道 Stack 上有哪些 roots，這就是 **Exact Roots**，完全安全且不需要掃描 stack memory）。

這樣您就擁有了：
*   比 `dumpster` 更快的分配速度 (BiBOP)。
*   比 `gc-arena` 更好寫的 API (HandleScope)。
*   比現在的 `rudo-gc` 更安全的記憶體管理 (Exact Roots)。

這才是 `rudo-gc` 成為 Rust 生態系首選 GC 的致勝關鍵。