# [Bug]: GcRootSet::snapshot 單一 heap 過濾導致跨執行緒 roots 丟失

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 多執行緒 GC 場景下必然發生 |
| **Severity (嚴重程度)** | Critical | 導致 live 物件被錯誤回收 (UAF) |
| **Reproducibility (復現難度)** | High | 需多執行緒 + tokio roots 場景 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRootSet::snapshot` (tokio/root.rs:127-143)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`GcRootSet::snapshot()` 函數接受一個 `&LocalHeap` 參數，並在內部使用 `find_gc_box_from_ptr(heap, ptr)` 來驗證每個 root 指針是否有效。然而，這種設計導致了一個嚴重的問題：當 GC 對多個執行緒的 heap 進行標記時，只有屬於當前 heap 的 roots 會被返回，來自其他執行緒的 roots 會被靜默過濾掉。

### 預期行為 (Expected Behavior)

`GcRootSet` 是文件所說的 "process-level singleton that maintains the collection of active GC roots across all tokio tasks and runtimes"。snapshot() 應該返回所有有效的 GC roots，不論它們屬於哪個執行緒。

### 實際行為 (Actual Behavior)

當 GC 調用 `GcRootSet::global().snapshot(heap)` 時：
- 如果 heap A 調用 snapshot，只返回 heap A 的 roots
- heap B、C 等其他執行緒的 roots 被 `find_gc_box_from_ptr(heap_A, ptr)` 過濾掉
- 這些被過濾掉的 roots 指向的物件可能被錯誤回收

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題出在 `tokio/root.rs:127-143`：

```rust
pub fn snapshot(&self, heap: &crate::heap::LocalHeap) -> Vec<usize> {
    let roots = self.roots.lock().unwrap();
    let valid_roots: Vec<usize> = roots
        .iter()
        .filter(|&&ptr| {
            // 這裡使用傳入的 heap 進行過濾
            unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8).is_some() }
        })
        .copied()
        .collect();
    // ...
}
```

`find_gc_box_from_ptr(heap, ptr)` 只在指針屬於傳入的 heap 地址範圍時返回 `Some`。當 GC 在多執行緒環境下运行时，每个线程的 heap 只会包含该线程自己分配的 roots，导致其他线程的 tokio roots 被忽略。

此函數被調用的位置：
- `gc/gc.rs:1184` - minor GC marking
- `gc/gc.rs:1317` - major GC marking  
- `gc/gc.rs:1486` - major GC marking
- `gc/gc.rs:1972` - incremental marking
- `gc/gc.rs:2050` - incremental marking
- `gc/incremental.rs:622` - snapshot phase

每次都是傳入當前正在處理的 heap，因此只會處理該 heap 對應執行緒的 roots。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要滿足的條件：
1. 多個 GC 執行緒（每個有自己的 LocalHeap）
2. 每個執行緒透過 `Gc::root_guard()` 註冊 tokio roots
3. GC 觸發並遍歷多個 heaps

理論 PoC：
```rust
#[test]
fn test_gcrootset_snapshot_multi_heap() {
    use std::thread;
    
    // Thread A creates Gc and registers root
    let gc_a = Gc::new(Data { value: 1 });
    let _guard_a = gc_a.root_guard();
    
    // Thread B creates Gc and registers root  
    let gc_b = Gc::new(Data { value: 2 });
    let _guard_b = gc_b.root_guard();
    
    // Trigger GC - roots from both threads should be preserved
    collect_full();
    
    // Access both - should not crash/UAF
    assert_eq!(gc_a.value, 1);
    assert_eq!(gc_b.value, 2); // May crash if root was incorrectly collected
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

方案一：移除 heap 參數，改用全局 heap 查詢
```rust
pub fn snapshot(&self) -> Vec<usize> {
    let roots = self.roots.lock().unwrap();
    let valid_roots: Vec<usize> = roots
        .iter()
        .filter(|&&ptr| {
            // 使用全局堆查找，不要過濾到特定 heap
            unsafe { crate::heap::find_gc_box_from_ptr_any_heap(ptr as *const u8).is_some() }
        })
        .copied()
        .collect();
    // ...
}
```

方案二：返回所有 roots，由調用方過濾
- 改變 API 設計，不在 snapshot 內做 heap 過濾
- 調用方負責對每個 root 驗證其有效性

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個設計違背了 GC root 追蹤的基本原則。roots 應該是全局的，不應該因為執行緒或 heap 的不同而被過濾。在 Chez Scheme 中，我們確保所有 root 指针在任何 GC 期間都被正確追蹤，即使它們分佈在不同的執行緒中。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。當 live 物件被錯誤回收後，任何後續訪問都會導致 use-after-free。這在 Rust 的記憶體安全承諾中是不可接受的。

**Geohot (Exploit 觀點):**
攻擊者可以刻意建立多執行緒場景，利用這個 bug 觸發 UAF，然後通過越界寫入或類似技術利用被錯誤回收的記憶體進行 exploit。這是一個可靠的記憶體破壞向量。
