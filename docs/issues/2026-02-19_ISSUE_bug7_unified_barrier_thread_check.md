# [Bug]: unified_write_barrier 缺少執行緒所有權驗證

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當從不同執行緒調用 barrier 時觸發 |
| **Severity (嚴重程度)** | High | 可能導致不正確的 barrier 行為或記憶體損壞 |
| **Reproducibility (復現難度)** | Low | 需要從不同執行緒調用 barrier |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `unified_write_barrier`, `incremental_write_barrier`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`unified_write_barrier` 和 `incremental_write_barrier` 函數缺少執行緒所有權檢查，與 `gc_cell_validate_and_barrier` 不同。

當 `gc_cell_validate_and_barrier` 被調用時，它會驗證：
```rust
let owner = (*h).owner_thread;
assert!(
    owner == 0 || owner == current,
    "Thread safety violation..."
);
```

但 `unified_write_barrier` 沒有這個檢查，導致：
1. 可能會在錯誤的 heap 上執行 barrier 邏輯
2. 執行緒安全不變性被破壞
3. 當從不同執行緒調用時可能導致記憶體損壞

### 預期行為
- 所有 barrier 函數應該驗證執行緒所有權
- 當從不同執行緒調用時應該 panic 或返回錯誤

### 實際行為
- `unified_write_barrier` 沒有執行緒檢查
- 可能導致在錯誤的 heap 上執行操作

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `heap.rs:2637-2687` 的 `unified_write_barrier` 函數中：

```rust
pub fn unified_write_barrier(ptr: *const u8, incremental_active: bool) {
    // ... 沒有執行緒所有權檢查！
    
    with_heap(|heap| {
        unsafe {
            let header = ptr_to_page_header(ptr);
            
            if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
                return;
            }
            // ... 繼續執行 barrier 邏輯
        }
    });
}
```

對比 `gc_cell_validate_and_barrier` (`heap.rs:2556-2628`)：

```rust
pub fn gc_cell_validate_and_barrier(ptr: *const u8, context: &str, incremental_active: bool) {
    let current = get_thread_id();
    
    with_heap(|heap| {
        // ...
        
        unsafe {
            let header = ptr_to_page_header(ptr);
            let owner = (*h).owner_thread;
            
            // 執行緒檢查存在！
            assert!(
                owner == 0 || owner == current,
                "Thread safety violation..."
            );
        }
    });
}
```

問題：
1. `unified_write_barrier` 缺少 `owner_thread` 檢查
2. 沒有 `get_thread_id()` 調用
3. 沒有執行緒安全斷言

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, collect_full};
use std::thread;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let data = Gc::new(Data { value: 42 });
    
    let handle = thread::spawn(move || {
        // 從不同執行緒調用 borrow_mut，觸發 write barrier
        // 這應該觸發執行緒安全斷言，但目前不會
        // let mut borrowed = data.borrow_mut();
        // *borrowed = 100;
    });
    
    handle.join().unwrap();
}
```

注意：這個 bug 可能需要通過 `GcThreadSafeCell` 或其他機制從不同執行緒調用 write barrier 來觸發。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：添加執行緒檢查到 unified_write_barrier

在 `unified_write_barrier` 开头添加檢查：

```rust
pub fn unified_write_barrier(ptr: *const u8, incremental_active: bool) {
    if ptr.is_null() {
        return;
    }

    let ptr_addr = ptr as usize;
    let current = get_thread_id();  // 添加

    with_heap(|heap| {
        if ptr_addr < heap.min_addr || ptr_addr > heap.max_addr {
            return;
        }

        unsafe {
            let header = ptr_to_page_header(ptr);
            
            if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
                return;
            }

            // 添加執行緒所有權檢查
            let owner = (*header.as_ptr()).owner_thread;
            assert!(
                owner == 0 || owner == current,
                "Thread safety violation in write barrier"
            );
            
            // ... 繼續執行 barrier 邏輯
        }
    });
}
```

### 方案 2：重構共享驗證邏輯

提取共享的驗證邏輯到一个函数：

```rust
unsafe fn validate_barrier_access(ptr: *const u8) -> (*mut PageHeader, bool) {
    let header = ptr_to_page_header(ptr);
    let current = get_thread_id();
    let owner = (*header.as_ptr()).owner_thread;
    
    let is_valid = owner == 0 || owner == current;
    (header, is_valid)
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在傳統的 GC 實現中，通常不允許從不同執行緒直接訪問堆對象。rudo-gc 的執行緒本地分配模型需要明確的所有權邊界，確保每個執行緒只能訪問自己分配的物件。

**Rustacean (Soundness 觀點):**
這是執行緒安全問題。缺少執行緒檢查可能導致數據競爭和記憶體損壞。在 Rust 的內存安全保證下，這種行為是不可接受的。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過：
1. 構造跨執行緒的 barrier 調用
2. 破壞 heap 數據結構
3. 可能實現任意記憶體寫入

