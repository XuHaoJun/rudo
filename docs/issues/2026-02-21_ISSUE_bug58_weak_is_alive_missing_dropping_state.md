# [Bug]: Weak::is_alive() 缺少 dropping_state 檢查導致不一致行為

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在物件正在被 drop 時調用 is_alive() |
| **Severity (嚴重程度)** | Medium | API 不一致導致邏輯錯誤，非記憶體安全問題 |
| **Reproducibility (復現難度)** | Low | 可透過比對程式碼發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::is_alive()` (`ptr.rs:1662-1669`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Weak::is_alive()` 應該與 `Weak::upgrade()` 具有一致的行為。當物件正在被 drop (`dropping_state() != 0`) 時，兩者都應該返回表示物件不可存活的值：
- `is_alive()` 應返回 `false`
- `upgrade()` 應返回 `None`

### 實際行為 (Actual Behavior)

目前 `Weak::is_alive()` 只檢查 `has_dead_flag()`，但沒有檢查 `dropping_state()`：

```rust
// ptr.rs:1662-1669
pub fn is_alive(&self) -> bool {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return false;
    };
    // 問題：只檢查 has_dead_flag()，沒有檢查 dropping_state()!
    unsafe { !(*ptr.as_ptr()).has_dead_flag() }
}
```

相比之下，`Weak::upgrade()` 正確地檢查了兩者：

```rust
// ptr.rs:1500-1507
loop {
    if gc_box.has_dead_flag() {  // ✓ 檢查
        return None;
    }

    if gc_box.dropping_state() != 0 {  // ✓ 檢查
        return None;
    }
    // ...
}
```

這導致當物件正在被 drop 時，`is_alive()` 返回 `true`，但 `upgrade()` 返回 `None`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1662-1669`，`is_alive()` 函數只檢查 `has_dead_flag()`：
```rust
unsafe { !(*ptr.as_ptr()).has_dead_flag() }
```

但漏掉了 `dropping_state() != 0` 的檢查。

正確的實現應該同時檢查兩者：
1. `has_dead_flag()` - 物件是否被標記為死亡
2. `dropping_state() != 0` - 物件是否正在被 drop 過程中

這與 bug42 (`Weak::try_upgrade()` 缺少 dropping_state 檢查) 和 bug52 (`Weak::strong_count()` 缺少 dropping_state 檢查) 是相同的模式問題。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, collect_full};
use std::rc::Rc;
use std::cell::Cell;
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 建立Rc追蹤dropping_state
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    // 使用Rc來手動控制drop時機
    let drop_triggered = Arc::new(AtomicBool::new(false));
    let drop_triggered_clone = drop_triggered.clone();
    
    // 啟動執行緒來觸發drop
    let handle = thread::spawn(move || {
        drop_triggered_clone.store(true, Ordering::Relaxed);
        drop(gc);  // 開始drop過程
    });
    
    // 等待另一執行緒開始drop
    while !drop_triggered.load(Ordering::Relaxed) {
        thread::yield_now();
    }
    
    // 這個時間點 dropping_state 可能 != 0
    let is_alive_result = weak.is_alive();
    let upgrade_result = weak.upgrade();
    
    println!("is_alive() = {}", is_alive_result);
    println!("upgrade() = {:?}", upgrade_result.is_some());
    
    // 預期：兩者都應該返回 false/None
    // 實際：is_alive() 可能返回 true，但 upgrade() 返回 None
    
    handle.join().unwrap();
}
```

注意：這個 bug 與 bug9 (`is_alive()` 不檢查 ref_count) 是不同的問題。bug9 已經存在，這個新 bug 是關於 `dropping_state` 檢查的缺失。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `ptr.rs:1662-1669` 處修改 `is_alive()` 方法：

```rust
pub fn is_alive(&self) -> bool {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return false;
    };

    unsafe {
        // 檢查 has_dead_flag()
        if (*ptr.as_ptr()).has_dead_flag() {
            return false;
        }
        // 檢查 dropping_state()
        if (*ptr.as_ptr()).dropping_state() != 0 {
            return false;
        }
        true
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在引用計數 GC 中，`dropping_state` 是用來防止在物件 drop 過程中建立新強引用的關鍵機制。當物件正在被 drop 時（`dropping_state != 0`），即使 `ref_count > 0`，也不應該允許建立新的強引用。這是因為現有的強引用將會完成 drop 流程，屆時物件會被釋放。`is_alive()` 和 `upgrade()` 應該具有一致的語義，否則會造成 API 使用上的困惑。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題，因為 `is_alive()` 本身是一個「非確定性」的檢查（文件中已說明）。但這是 API 一致性問題 - 當 `upgrade()` 返回 `None` 時，`is_alive()` 應該也返回 `false`，否則會造成邏輯錯誤。

**Geohot (Exploit 攻擊觀點):**
雖然這不是安全性問題，但不一致的 API 可能被利用來构造複雜的 bug。例如：
1. 攻擊者可能利用 `is_alive()` 返回 `true` 但 `upgrade()` 返回 `None` 的時間窗口
2. 在並發場景下，這種不一致可能導致難以預測的行為
3. 攻擊者可能利用這一點構造依賴時序的複雜攻擊

(End of file - total 186 lines)
