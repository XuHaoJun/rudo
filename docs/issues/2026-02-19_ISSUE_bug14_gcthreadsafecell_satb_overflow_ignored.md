# [Bug]: GcThreadSafeCell::borrow_mut() 忽略 record_satb_old_value 返回值導致 SATB 不變性破壞

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 SATB buffer 溢出時觸發 |
| **Severity (嚴重程度)** | Critical | 導致 SATB 不變性破壞，可能造成 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要大量 GC 指針的 GcThreadSafeCell 觸發 buffer 溢出 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcThreadSafeCell::borrow_mut`, `record_satb_old_value`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`GcThreadSafeCell::borrow_mut()` 方法在記錄 SATB 舊值時忽略 `record_satb_old_value()` 的返回值。當 SATB buffer 溢出時，函數返回 `false` 表示需要 fallback，但此返回值被忽略，導致：

1. 舊值未被正確記錄到 SATB buffer
2. 增量標記無法保留應該保留的物件
3. 可能導致 use-after-free

### 預期行為
- 當 `record_satb_old_value()` 返回 `false` 時，應該觸發 fallback 或記錄到溢出緩衝區
- 應該與 `GcCell::borrow_mut()` 的行為一致

### 實際行為
- `record_satb_old_value()` 的返回值被忽略 (`let _ = ...`)
- 即使函數返回 `false`，程式碼也繼續執行
- SATB 不變性被破壞

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cell.rs:919-922` 的 `GcThreadSafeCell::borrow_mut()` 實作中：

```rust
if crate::heap::try_with_heap(|heap| {
    for gc_ptr in &gc_ptrs {
        let _ = heap.record_satb_old_value(*gc_ptr);  // 問題：忽略返回值！
    }
})
.is_some()
{
    // Heap available, SATB recorded in thread-local buffer
} else {
    // No GC heap on this thread, use cross-thread buffer
    for gc_ptr in gc_ptrs {
        crate::heap::LocalHeap::push_cross_thread_satb(gc_ptr);
    }
}
```

對比 `GcCell::borrow_mut()` (`cell.rs:166-173`) 的正確實現：

```rust
crate::heap::with_heap(|heap| {
    for gc_ptr in gc_ptrs {
        if !heap.record_satb_old_value(gc_ptr) {  // 正確：檢查返回值！
            break;
        }
    }
});
```

問題：
1. `record_satb_old_value()` 返回 `false` 表示 buffer 溢出需要 fallback
2. `GcThreadSafeCell` 忽略此返回值，導致 fallback 未被觸發
3. 與 `GcCell::borrow_mut()` 行為不一致

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace, collect_full};
use std::sync::Arc;
use std::thread;

#[derive(Trace)]
struct Data {
    values: Vec<Gc<i32>>,
}

fn main() {
    // 降低 SATB buffer 容量以更容易觸發溢出
    // 這需要通過內部 API 或測試環境配置
    
    // 創建大量 GC 指針的 GcThreadSafeCell
    let cell = Gc::new(GcThreadSafeCell::new(Data {
        values: (0..100).map(|i| Gc::new(i)).collect(),
    }));
    
    // 確保增量標記 active
    // ... 觸發 incremental marking
    
    // 執行大量 OLD -> YOUNG 寫入
    for _ in 0..1000 {
        let mut guard = cell.borrow_mut();
        guard.values.push(Gc::new(999));
    }
    
    // 由於 SATB buffer 溢出未被處理
    // 某些物件可能被錯誤回收
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：檢查返回值並觸發 fallback（推薦）

```rust
if crate::heap::try_with_heap(|heap| {
    for gc_ptr in &gc_ptrs {
        if !heap.record_satb_old_value(*gc_ptr) {
            // Buffer 溢出，請求 fallback
            crate::gc::incremental::IncrementalMarkState::global()
                .request_fallback(crate::gc::incremental::FallbackReason::SatbBufferOverflow);
            break;
        }
    }
    true  // 返回 true 表示 heap 可用
})
.is_some()
{
    // Heap available, SATB recorded in thread-local buffer
} else {
    // No GC heap on this thread, use cross-thread buffer
    for gc_ptr in gc_ptrs {
        crate::heap::LocalHeap::push_cross_thread_satb(gc_ptr);
    }
}
```

### 方案 2：記錄到溢出緩衝區

```rust
if crate::heap::try_with_heap(|heap| {
    for gc_ptr in &gc_ptrs {
        if !heap.record_satb_old_value(*gc_ptr) {
            // 記錄到溢出緩衝區
            heap.record_satb_overflow(*gc_ptr);
        }
    }
})
.is_some()
{
    // Heap available
} else {
    // No GC heap on this thread
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB (Snapshot-At-The-Beginning) 不變性是增量標記的基礎。當舊值未被正確記錄時，標記階段可能無法保留所有在標記開始時可達的物件。這是增量標記中的經典問題，需要確保所有舊值都被正確記錄。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。當 SATB 不變性被破壞時，某些物件可能被錯誤回收，導致後續存取時發生 use-after-free。這是未定義行為。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過：
1. 構造大量 OLD -> YOUNG 引用
2. 觸發 SATB buffer 溢出
3. 利用未處理的溢出導致物件被錯誤回收
4. 實現記憶體佈局控制

