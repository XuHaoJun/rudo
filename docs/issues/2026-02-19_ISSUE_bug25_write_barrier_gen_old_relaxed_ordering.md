# [Bug]: Write Barrier 中 GEN_OLD_FLAG 讀取使用 Relaxed Ordering 導致潛在 Race Condition

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要多執行緒並發寫入同一個 GcCell/GcRwLock 才會觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致 barrier 行為不正確 |
| **Reproducibility (復現難度)** | Low | 需要精確的時序控制才能重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `unified_write_barrier`, `generational_write_barrier`, `incremental_write_barrier` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在 write barrier 中讀取 `GEN_OLD_FLAG` 時，應該使用適當的 atomic ordering 來確保正確的記憶體同步，特別是在多執行緒環境中。

### 實際行為 (Actual Behavior)
在 `unified_write_barrier` 和相關函數中，`weak_count_raw()` 使用 `Ordering::Relaxed` 讀取，這可能導致：

```rust
// heap.rs:2671-2676
let gc_box_addr =
    (header_page_addr + header_size + index * block_size) as *const GcBox<()>;
let wc = (*gc_box_addr).weak_count_raw();  // 使用 Relaxed ordering!
if (wc & GcBox::<()>::GEN_OLD_FLAG) == 0 {
    return;  // 可能錯誤地跳過 barrier
}
```

`weak_count_raw()` 的實現（ptr.rs:190-192）：
```rust
pub fn weak_count_raw(&self) -> usize {
    self.weak_count.load(Ordering::Relaxed)  // Relaxed ordering!
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `weak_count_raw()` 使用 `Ordering::Relaxed` 來讀取，這在以下場景中可能出問題：

1. **多執行緒並發寫入**：當一個執行緒在 write barrier 中讀取 `GEN_OLD_FLAG` 時
2. **另一個執行緒正在修改**：同時修改同一個物件的 `weak_count` 欄位（例如，設置 `GEN_OLD_FLAG`）

使用 `Relaxed` ordering 的問題：
- 不提供跨執行緒的同步保證
- 可能讀取到過期的值
- CPU 快取可能包含陳舊的資料

正確的行為應該是：
- 當讀取 `GEN_OLD_FLAG` 為 0 時，應該執行 barrier（物件是 young）
- 但由於 Relaxed ordering，可能錯誤地讀到 1 並跳過 barrier
- 這導致 OLD→YOUNG 引用不被追蹤

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcThreadSafeCell, Trace};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 啟用 incremental marking
    let config = rudo_gc::gc::incremental::IncrementalConfig {
        enabled: true,
        slice_timeout_ms: 10,
        ..Default::default()
    };
    rudo_gc::gc::incremental::IncrementalMarkState::global().set_config(config);
    
    // 使用 GcThreadSafeCell
    let cell = Gc::new(GcThreadSafeCell::new(Data { value: 0 }));
    
    // 多個執行緒並發寫入
    let handles: Vec<_> = (0..4).map(|_| {
        thread::spawn(move || {
            for _ in 0..10000 {
                let mut guard = cell.borrow_mut();
                guard.value += 1;
            }
        })
    }).collect();
    
    for handle in handles {
        handle.join().unwrap();
    }
}
```

關鍵問題：在高並發場景下，write barrier 可能會因為 Relaxed ordering 而讀取到錯誤的 GEN_OLD_FLAG 值。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：使用更強的 Ordering
```rust
// 在 write barrier 中
let wc = (*gc_box_addr).weak_count.load(Ordering::Acquire);
```

選項 2：添加新的函數專門用於 barrier 場景
```rust
impl GcBox {
    /// 專門用於 barrier 的讀取，使用 Acquire ordering
    pub fn gen_old_flag_for_barrier(&self) -> bool {
        (self.weak_count.load(Ordering::Acquire) & Self::GEN_OLD_FLAG) != 0
    }
}
```

選項 3：在 barrier 開始時添加 fence
```rust
std::sync::atomic::fence(Ordering::AcqRel);
// 然後讀取 weak_count_raw
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在generational GC 中，精確地確定物件的世代至關重要。如果錯誤地跳過 barrier，OLD→YOUNG 引用將不會被追蹤，導致minor collection可能錯誤地回收young物件。這是一個正確性問題，可能導致記憶體洩漏或 use-after-free。

**Rustacean (Soundness 觀點):**
Relaxed ordering 在這種情況下是一個微妙的問題。雖然不是立即的 UB，但可能導致違反 GC 不變量。在Rust的記憶體模型中，Relaxed ordering 不提供跨執行緒的可見性保證，這可能導致非預期的行為。

**Geohot (Exploit 觀點):**
雖然這個 bug 需要精確的時序控制，但攻擊者可能利用這個不確定性來：
1. 誘使 GC 錯誤地跳過 barrier
2. 導致minor collection 回收活跃对象
3. 實現記憶體錯誤

這個問題在單執行緒環境下不會出現，但在多執行緒環境下可能導致微妙的記憶體錯誤。

---

## Resolution

**2026-02-21** — Applied 選項 1 + 2 (Acquire/Release ordering):

- Added `has_gen_old_flag()` in `ptr.rs` using `Ordering::Acquire` for barrier reads.
- Changed `set_gen_old()` from `fetch_or(Relaxed)` to `fetch_or(Release)` so GC promotion and mutator barrier synchronize correctly.
- Replaced all write-barrier `weak_count_raw()` + `GEN_OLD_FLAG` checks in `heap.rs` with `has_gen_old_flag()`.
- Affected: `simple_write_barrier`, `gc_cell_validate_and_barrier`, `unified_write_barrier`, `incremental_write_barrier`.
- Build, clippy, and `./test.sh` pass.

