# [Bug]: Weak::upgrade() ref_count Relaxed 載入導致 TOCTOU Use-After-Free

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 任何並發場景下 weak upgrade 與 dec_ref 同時執行 |
| **Severity (嚴重程度)** | Critical | 可能導致 use-after-free 和記憶體腐敗 |
| **Reproducibility (復現難度)** | Medium | 需要精確的執行時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::upgrade()`, `Weak::try_upgrade()`, `GcBoxWeakRef::upgrade()`
- **OS / Architecture:** Linux x86_64, All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`Weak::upgrade()` 函數存在 TOCTOU (Time-Of-Check-Time-Of-Use) 競爭條件。程式碼使用 `Ordering::Relaxed` 載入 `ref_count`，然後檢查是否為 0 再執行 CAS 增量。這導致在載入和 CAS 之間，另一個執行緒可能已將 ref_count 遞減至 0 並開始釋放物件，但当前线程仍能看到舊值並成功遞增，導致對已釋放物件的引用。

### 預期行為
- `upgrade()` 應該在物件已死亡時返回 `None`
- 不應該返回對已釋放物件的引用

### 實際行為
1. Thread A 載入 `ref_count = 1` (使用 Relaxed ordering)
2. Thread B 遞減 `ref_count` 至 0 (最後一個引用) 並開始 drop 物件
3. Thread A 檢查 `current_count == 0` - 看到 1 (過期值)，通過檢查
4. Thread A 執行 CAS 從 1 遞增至 2 - 成功!
5. Thread A 現在擁有一個已 drop 物件的 "Gc" - **Use-After-Free!**

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs` 的三個位置，程式碼使用 `Ordering::Relaxed` 載入 `ref_count`：

**1. `Weak::upgrade()` - ptr.rs:1489**
```rust
let current_count = gc_box.ref_count.load(Ordering::Relaxed);
if current_count == 0 {
    return None;
}
// ... 後續 CAS 可能成功，但物件已死亡
```

**2. `Weak::try_upgrade()` - ptr.rs:1567**
```rust
let current_count = gc_box.ref_count.load(Ordering::Relaxed);
if current_count == 0 || current_count == usize::MAX {
    return None;
}
```

**3. `GcBoxWeakRef::upgrade()` - ptr.rs:509**
```rust
let current_count = gc_box.ref_count.load(Ordering::Relaxed);
```

問題在於 Relaxed ordering 不提供任何同步保證，無法確保我們看到其他執行緒對 ref_count 的最新修改。應該使用 `Acquire` ordering 來確保我們看到所有之前的遞減操作。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, collect_full};
use std::sync::Arc;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    let upgrade_called = Arc::new(AtomicBool::new(false));
    let upgrade_called_clone = upgrade_called.clone();
    
    let handle = thread::spawn(move || {
        // 等待信號
        while !upgrade_called_clone.load(Ordering::Relaxed) {
            thread::yield();
        }
        
        // 嘗試 upgrade - 可能會 UAF!
        let strong = weak.upgrade();
        if strong.is_some() {
            println!("Upgrade succeeded (UAF!): {}", strong.unwrap().value);
        } else {
            println!("Upgrade correctly returned None");
        }
    });
    
    // Drop 最後的 strong reference
    drop(gc);
    
    // 觸發 GC 回收
    collect_full();
    
    // 通知另一個執行緒執行 upgrade
    upgrade_called.store(true, Ordering::Relaxed);
    
    handle.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將所有三處的 `Ordering::Relaxed` 改為 `Ordering::Acquire`：

**ptr.rs:1489 - Weak::upgrade()**
```rust
// 改前：
let current_count = gc_box.ref_count.load(Ordering::Relaxed);

// 改後：
let current_count = gc_box.ref_count.load(Ordering::Acquire);
```

**ptr.rs:1567 - Weak::try_upgrade()**
```rust
// 改前：
let current_count = gc_box.ref_count.load(Ordering::Relaxed);

// 改後：
let current_count = gc_box.ref_count.load(Ordering::Acquire);
```

**ptr.rs:509 - GcBoxWeakRef::upgrade()**
```rust
// 改前：
let current_count = gc_box.ref_count.load(Ordering::Relaxed);

// 改後：
let current_count = gc_box.ref_count.load(Ordering::Acquire);
```

使用 `Acquire` ordering 可以確保：
1. 我們看到所有之前的 ref_count 遞減操作
2. 我們看到物件狀態的最新變化
3. 防止在檢查和 CAS 之間的時間視窗內物件被回收

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是一個經典的 reference counting race condition。在傳統的 RC 實現中，upgrade 操作需要確保原子性 - 不能簡單地"檢查然後遞增"。正確的做法是使用 compare-and-swap 並依賴其失敗路徑來處理並發修改。使用 Acquire ordering 是必要的，以確保與遞減執行緒的同步。

**Rustacean (Soundness 觀點):**
這是一個明確的記憶體安全問題。使用 Relaxed ordering 載入計數然後解引用物件是危險的。如果物件已被釋放，解引用指標是未定義行為。必須修復以確保記憶體安全。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過構造精確時序的 weak upgrade 調用來洩露已釋放記憶體的內容：
1. 建立一個即將被回收的物件
2. 在 dec_ref 執行的同時觸發 weak upgrade
3. 利用過期的計數值來 access 已釋放的記憶體
4. 這可用於資訊洩露或進一步的記憶體腐敗攻擊
