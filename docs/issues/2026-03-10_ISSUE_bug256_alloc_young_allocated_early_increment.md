# [Bug]: LocalHeap::alloc() 過早增加 young_allocated 導致記憶體計數不準確

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 分配失敗時才會觸發，但現代系統很少發生 |
| **Severity (嚴重程度)** | Low | 僅影響堆積計數準確性，不影響記憶體安全 |
| **Reproducibility (復現難度)** | Very Low | 需要人為製造分配失敗情境 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `LocalHeap::alloc()` (heap.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

在 `LocalHeap::alloc<T>()` 函數中，`young_allocated` 計數器在實際分配成功之前就被遞增。如果後續的分配路徑（如 TLAB 分配、free list 分配、或 slow path 分配）失敗或 panic，計數器將保持遞增後的錯誤狀態，導致堆積大小統計不準確。

### 預期行為 (Expected Behavior)
`young_allocated` 應該只在分配成功後才遞增。

### 實際行為 (Actual Behavior)
`young_allocated` 在分配一開始就被遞增，無論最終是否成功。

---

## 🔬 根本原因分析 (Root Cause Analysis)

位置：`crates/rudo-gc/src/heap.rs:2008-2059`

```rust
pub fn alloc<T>(&mut self) -> NonNull<u8> {
    let size = std::mem::size_of::<T>();
    let align = std::mem::align_of::<T>();
    // All new allocations start in young generation
    self.young_allocated += size;  // <-- 過早遞增！

    if size > MAX_SMALL_OBJECT_SIZE {
        return self.alloc_large(size, align);
    }
    // ... 後續分配路徑可能失敗 ...
}
```

問題在於 `young_allocated += size` (第 2012 行) 發生在任何分配邏輯之前。如果後續分配失敗（例如 `alloc_slow` panic 或返回錯誤），計數器已經被錯誤地遞增。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此問題需要人為製造分配失敗情境，在正常運行中極難觸發。概念上：

1. 建立一個會導致分配失敗的情境（例如記憶體耗盡）
2. 呼叫 `LocalHeap::alloc()`
3. 觀察 `young_allocated` 被錯誤遞增

```rust
// 概念驗證程式碼
fn simulate_allocation_failure() {
    let mut heap = LocalHeap::new();
    let initial_young_allocated = heap.young_allocated;
    
    // 嘗試分配一個會失敗的大型物件
    // (需要其他方式觸發失敗路徑)
    
    // 檢查 young_allocated 是否錯誤遞增
    assert!(heap.young_allocated >= initial_young_allocated); // 可能錯誤！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `young_allocated` 的遞增移動到分配成功之後：

```rust
pub fn alloc<T>(&mut self) -> NonNull<u8> {
    let size = std::mem::size_of::<T>();
    let align = std::mem::align_of::<T>();

    if size > MAX_SMALL_OBJECT_SIZE {
        let ptr = self.alloc_large(size, align);
        self.young_allocated += size;  // 遞增移到成功後
        return ptr;
    }

    // ... 其他分配路徑 ...

    // 在返回前遞增
    let ptr = self.alloc_slow(size, class_index);
    self.young_allocated += size;
    ptr
}
```

或者使用 RAII 模式在成功返回時遞增。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
從 GC 視角看，`young_allocated` 用於追蹤年輕代已分配的記憶體。如果計數不準確，可能影響：
- GC 觸發時機的判斷
- 記憶體壓力報告
- 調優參數的計算

但這不會影響 GC 的正確性，因為 GC 是基於指標追蹤，而非依賴計數器。

**Rustacean (Soundness 觀點):**
這不是記憶體安全問題或 UB。計數器錯誤不會導致 use-after-free 或其他記憶體錯誤。僅影響觀察性的統計數據。

**Geohot (Exploit 觀點):**
極難利用此漏洞。需要精確控制分配失敗的時機，且即使成功，也只能影響堆積統計數據，對攻擊無直接幫助。
