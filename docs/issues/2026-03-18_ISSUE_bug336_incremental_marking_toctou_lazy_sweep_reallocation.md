# [Bug]: Incremental Marking TOCTOU - Lazy Sweep Reallocation Between Two is_allocated Checks

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需要精確時序：lazy sweep 在兩次 is_allocated 檢查之間完成物件重新分配 |
| **Severity (嚴重程度)** | `Medium` | 導致新分配的物件被錯誤標記為 reachable，可能造成記憶體保留不當 |
| **Reproducibility (復現難度)** | `High` | 需要精確的執行時序，單執行緒難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Incremental Marking`, `scan_page_for_marked_refs`, `scan_page_for_unmarked_refs`
- **OS / Architecture:** `Linux x86_64`, `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在增量標記期間，只有在 mark phase 開始時已存在的物件應該被標記為 reachable。新分配的物件不應該被錯誤地標記。

### 實際行為 (Actual Behavior)
在 `scan_page_for_marked_refs` (incremental.rs:815-867) 和 `scan_page_for_unmarked_refs` (incremental.rs:954-996) 中，雖然代碼有兩次 `is_allocated` 檢查來防止 TOCTOU，但這個模式存在漏洞：

1. 第一次 `is_allocated` 檢查發現 slot 被 sweep（返回 false）
2. 在第一次和第二次檢查之間，lazy sweep **重新分配**該 slot 給新物件（is_allocated 變回 true）
3. 第二次 `is_allocated` 檢查通過（看到新物件）
4. 新物件被錯誤地推送到 worklist

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `scan_page_for_marked_refs` (incremental.rs:841-850)：
```rust
// 第一次檢查 - slot 已被 sweep
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    break;
}
// BUG: 第一次和第二次檢查之間，lazy sweep 可以重新分配 slot 給新物件！

// 第二次檢查 - 如果新物件被分配，通過檢查
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    break;
}
refs_found += 1;
// BUG: 新物件被錯誤地推送到 worklist
state.push_work(gc_box);
```

現有的修復（如 bug258, bug291）只處理了「sweep 後 slot 保持空」的狀況，沒有處理「sweep 後立即重新分配」的狀況。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要精確的時序控制來穩定重現：
1. 啟動增量標記 (mark phase 開始)
2. 建立 Gc<T> 並確保它在被掃描的 dirty page 中
3. Thread A 掃描 page，調用 `try_mark()` 成功
4. 第一次 `is_allocated` 檢查：slot 被 sweep（false）
5. Lazy sweep 運行，在同一 slot 分配新物件（is_allocated 變 true）
6. 第二次 `is_allocated` 檢查：通過（新物件被認為是 alive）
7. 新物件被錯誤地標記並推送到 worklist

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

需要一種機制來區分「原始物件」和「重新分配的物件」：
1. 在 PageHeader 或 slot 元資料中加入 timestamp 或 generation counter
2. 記錄 mark phase 開始時間
3. 標記時檢查物件是否在 mark phase 開始之後分配
4. 如果物件「太新」，即使 slot 目前是 allocated 狀態，也跳過標記

**可行的修復方案 - 使用 Generation 檢查 (2026-03-20 分析)：**

GcBox 已經有 `generation` 字段用於檢測 slot 重用（bug347）。當 slot 被 sweep 並重新分配時，`try_pop_from_page` 會調用 `increment_generation()`。

修復方案是在 `scan_page_for_marked_refs` 和 `scan_page_for_unmarked_refs` 中，在 `set_mark`/`try_mark` 成功後，立即保存 generation 值，然後在 `push_work` 之前驗證 generation 沒有改變：

```rust
// 在 set_mark/try_mark 成功後，立即讀取 generation
let marked_generation = unsafe { (*gc_box_ptr).generation() };

// 第一次 is_allocated 檢查
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    continue;
}

// 第二次 is_allocated 檢查
if !(*header).is_allocated(i) {
    (*header).clear_mark_atomic(i);
    continue;
}

// 在 push_work 之前，驗證 generation
let current_generation = unsafe { (*gc_box_ptr).generation() };
if current_generation != marked_generation {
    // Slot 被重用 - generation 改變了，不要 push
    (*header).clear_mark_atomic(i);
    continue;
}
```

**為什麼 generation 檢查有效：**
- 當 slot 被 sweep 並重新分配時，generation 會增加
- 如果在 `set_mark` 和 `push_work` 之间 slot 被重用，generation 會不同
- 這能正確區分「原始物件仍在 slot 中」和「新物件被分配到同一 slot」

**注意：** 需要確保 `dealloc` 不會遞增 generation（目前 `dealloc` 只清除 allocated_bitmap，不會遞增 generation，所以這個修復是有效的）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 源於增量標記和 lazy sweep 的交互。傳統的 mark-sweep GC 通常不會有這個問題，因為 sweep 和 mark 是分離的 phases。但在增量 GC 中，兩者可能並發執行。SATB (Snapshot-At-The-Beginning) 語義要求只標記 mark phase 開始時存在的物件，但目前的實現無法完全保證這一點。

**Rustacean (Soundness 觀點):**
這不是傳統意義上的 UB，但可能導致記憶體保留問題（memory retention issue）。新物件被錯誤標記會導致它們比預期更早晉升到 old generation，影響 generational GC 的效率。不過不會造成 use-after-free。

**Geohot (Exploit 觀點):**
如果要利用這個 bug，需要能夠控制 GC 時序。在真實場景中，可能通過精確的記憶體壓力控制和執行緒調度來觸發。雖然難度較高，但理論上可以通過這個 bug 影響 GC 的記憶體佈局，導致記憶體佔用高於預期。
