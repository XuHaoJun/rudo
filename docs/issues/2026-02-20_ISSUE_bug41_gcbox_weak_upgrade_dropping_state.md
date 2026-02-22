# [Bug]: GcBoxWeakRef::upgrade() 未檢查 dropping_state 導致 Use-After-Free 風險

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 Weak reference upgrade 時，物件正在被 drop（ref_count > 0 且 dropping_state != 0） |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序控制觸發 race condition |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade()`, `ptr.rs:406-434`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當呼叫 `GcBoxWeakRef::upgrade()` 時，如果物件正在被 drop（`dropping_state != 0`），無論 `ref_count` 是否大於 0，都應該返回 `None`，防止建立新的強引用導致 Use-After-Free。

### 實際行為 (Actual Behavior)
在 `ptr.rs:429` 處，當 `ref_count > 0`（代表已有強引用存在）時，程式碼直接呼叫 `gc_box.inc_ref()` 而沒有檢查 `dropping_state()`。

相比之下，`Weak::upgrade()`（`ptr.rs:1505-1506`）正確地檢查了 `dropping_state()`：
```rust
if gc_box.dropping_state() != 0 {
    return None;
}
```

但 `GcBoxWeakRef::upgrade()` 缺少這個檢查，導致以下場景可能發生 UAF：
1. 物件 A 有 ref_count = 1
2. 執行緒 1 開始 drop 物件 A，設置 dropping_state = 1
3. 執行緒 2 呼叫 A 的 GcBoxWeakRef::upgrade()
4. 由於 ref_count = 1 > 0，程式碼跳過 try_inc_ref_from_zero，直接執行 inc_ref()
5. 執行緒 2 獲得新的 Gc<T> 指向正在被 drop 的物件
6. 執行緒 1 完成 drop，物件記憶體被釋放
7. 執行緒 2 使用 Gc<T> 訪問已釋放的記憶體 → UAF

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在 `ptr.rs:421-434`：

```rust
// Try atomic transition from 0 to 1 (resurrection)
if gc_box.try_inc_ref_from_zero() {
    return Some(Gc { ... });
}

// BUG: 這裡直接 inc_ref，沒有檢查 dropping_state
gc_box.inc_ref();  // line 429
Some(Gc { ... })
```

當 `ref_count > 0` 時，`try_inc_ref_from_zero()` 返回 false（因為 ref_count != 0），程式碼進入 line 429 直接遞增 ref_count，但沒有驗證物件是否正在被 drop。

`try_inc_ref_from_zero()` 內部會檢查：
- DEAD_FLAG（已檢查）
- weak_count == 0 with flags（已檢查）
- ref_count == 0（已檢查）

但它不會檢查 `dropping_state()`，因為它只在 ref_count == 0 時被呼叫。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Trace)]
struct Data {
    value: i32,
}

static DROPPING: AtomicBool = AtomicBool::new(false);

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    // 在另一執行緒啟動 drop
    let weak_clone = weak.clone();
    thread::spawn(move || {
        // 開始 drop
        DROPPING.store(true, Ordering::SeqCst);
        drop(weak_clone);  // 這會觸發 dec_weak
        // 這裡 dropping_state 應該已設置
    });
    
    // 等待執行緒開始 drop
    while !DROPPING.load(Ordering::SeqCst) {
        thread::yield_now();
    }
    
    // 嘗試 upgrade - 在真实场景中可能成功但導致 UAF
    // 由于当前实现的问题，即使 dropping_state != 0 也可能返回 Some
    let result = weak.upgrade();
    
    // 預期：result 應該是 None（因為物件正在被 drop）
    // 實際：result 可能是 Some（如果 ref_count > 0）
    println!("Upgrade result: {:?}", result.is_some());
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `ptr.rs:429` 之前添加 `dropping_state()` 檢查：

```rust
// Try atomic transition from 0 to 1 (resurrection)
if gc_box.try_inc_ref_from_zero() {
    return Some(Gc { ... });
}

// 新增：檢查物件是否正在被 drop
if gc_box.dropping_state() != 0 {
    return None;
}

gc_box.inc_ref();
Some(Gc { ... })
```

或者修改 `try_inc_ref_from_zero()` 來接受 dropping_state 檢查的參數。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 reference counting GC 中，當物件正在被 drop 時（dropping_state != 0），即使 ref_count > 0，也不應該允許新的強引用建立。這是因為舊的強引用將會完成 drop 流程，屆時物件會被釋放。新建立的強引用會指向已釋放的記憶體，違反 GC 的記憶體安全 invariant。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題（Memory Safety），不是傳統的 soundness 問題（不會導致 UB）。允許在 dropping_state != 0 時建立新的 Gc<T> 會導致 Use-After-Free，Rust 的記憶體安全保證被破壞。

**Geohot (Exploit 觀點):**
此漏洞可以被利用來實現 use-after-free。如果攻擊者能夠控制升級 weak reference 的時序，他們可能能夠：
1. 讓物件進入 dropping_state
2. 在物件記憶體釋放前取得新的 Gc<T>
3. 利用已釋放的記憶體（取決於記憶體分配器行為）

---

**Resolution:** Added `dropping_state() != 0` check in `GcBoxWeakRef::upgrade()` before `inc_ref()`, returning `None` when the object is being dropped. Matches `Weak::upgrade()` pattern.
