# [Bug]: GcCell::borrow_mut() 缺少 SATB buffer overflow fallback 請求

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 incremental marking 期間對 GcCell 進行大量 mutation |
| **Severity (嚴重程度)** | High | 導致 SATB 不變性破壞，可能導致物件被錯誤回收 |
| **Reproducibility (復現難度)** | Medium | 需要大量 GC 指針的 GcCell 觸發 buffer 溢出 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCell::borrow_mut()` in `cell.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
當 SATB buffer 溢出時，`GcCell::borrow_mut()` 應該與 `GcThreadSafeCell::borrow_mut()` 行為一致，請求 GC fallback 以確保 SATB 不變性。

### 實際行為
`GcCell::borrow_mut()` (cell.rs:166-173) 在記錄 SATB 舊值時，忽略 `record_satb_old_value()` 的返回值。當 SATB buffer 溢出時，函數返回 `false` 表示需要 fallback，但此返回值被忽略，導致 fallback 未被觸發。

相比之下，`GcThreadSafeCell::borrow_mut()` (cell.rs:788-792) 正確地在 SATB buffer 溢出時請求 fallback：
```rust
if !heap.record_satb_old_value(*gc_ptr) {
    crate::gc::incremental::IncrementalMarkState::global()
        .request_fallback(
            crate::gc::incremental::FallbackReason::SatbBufferOverflow,
        );
    break;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題程式碼
`crates/rudo-gc/src/cell.rs:166-173`

```rust
// GcCell::borrow_mut() - BUG: 沒有請求 fallback
crate::heap::with_heap(|heap| {
    for gc_ptr in gc_ptrs {
        if !heap.record_satb_old_value(gc_ptr) {
            break;
        }
    }
});

// GcThreadSafeCell::borrow_mut() - 正確實作
if crate::heap::try_with_heap(|heap| {
    for gc_ptr in &gc_ptrs {
        if !heap.record_satb_old_value(*gc_ptr) {
            crate::gc::incremental::IncrementalMarkState::global()
                .request_fallback(
                    crate::gc::incremental::FallbackReason::SatbBufferOverflow,
                );
            break;
        }
    }
    true
})
.is_some()
{
    // Heap available, SATB recorded in thread-local buffer
}
```

### 邏輯缺陷

1. `GcCell::borrow_mut()` 檢查 `record_satb_old_value()` 返回值，但發現 `false` 時只 break，沒有請求 fallback
2. 這導致當 SATB buffer 溢出時，GC 不會收到 fallback 請求
3. SATB 不變性被破壞：應該被標記為 OLD 的物件可能被錯誤回收

### 與 bug14 的關係

bug14 報告了 `GcThreadSafeCell::borrow_mut()` 忽略返回值（實際上當時確實如此）。現在 `GcThreadSafeCell` 已修復，但 `GcCell` 仍有相同問題。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, GcCapture, collect_full, set_incremental_config, IncrementalConfig};
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Trace, GcCapture)]
struct Data {
    pointers: Vec<Gc<RefCell<i32>>>,
}

fn main() {
    // 啟用 incremental marking
    set_incremental_config(IncrementalConfig {
        enabled: true,
        dirty_pages_threshold: 1000,
        slice_duration_ns: 1_000_000,
    });

    // 創建大量 GC 指針的 GcCell
    let pointers: Vec<Gc<RefCell<i32>>> = (0..1000)
        .map(|i| Gc::new(RefCell::new(i as i32)))
        .collect();
    
    let cell = Gc::new(GcCell::new(Data { pointers }));
    
    // 觸發 incremental marking
    // ... (略)
    
    // 大量 mutation 導致 SATB buffer 溢出
    for _ in 0..10000 {
        let mut guard = cell.borrow_mut();
        guard.pointers.push(Gc::new(RefCell::new(999)));
    }
    
    // 由於沒有請求 fallback，SATB 不變性被破壞
    // 可能導致物件被錯誤回收
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

修改 `GcCell::borrow_mut()` 以請求 fallback：

```rust
crate::heap::with_heap(|heap| {
    for gc_ptr in gc_ptrs {
        if !heap.record_satb_old_value(gc_ptr) {
            crate::gc::incremental::IncrementalMarkState::global()
                .request_fallback(
                    crate::gc::incremental::FallbackReason::SatbBufferOverflow,
                );
            break;
        }
    }
});
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB (Snapshot-At-The-Beginning) 是一個重要的不變性：所有在 GC 開始時存活的物件必須被標記。當 SATB buffer 溢出時，fallback 機制確保 GC 進入 STW 模式以維護這個不變性。GcCell 忽略返回值破壞了這個關鍵機制，可能導致物件被錯誤地 Sweep。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。當 SATB 不變性被破壞時，在 mutation 過程中建立的新引用可能不會被正確追蹤，導致原本應該存活的物件被錯誤回收。存取已回收的記憶體會導致 use-after-free。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以透過觸發 SATB buffer 溢出來利用這個 bug：1. 創建一個包含敏感數據的物件
2. 觸發 incremental marking
3. 透過 GcCell 大量 mutation 導致 buffer 溢出4. 由於 fallback 未被請求，GC 可能錯誤回收物件
5. 攻擊者可以讀取已回收記憶體中的殘餘數據
