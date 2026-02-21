# [Bug]: Weak::is_alive() 不檢查 ref_count 導致不一致行為

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 Weak ref 存在但沒有強引用時觸發 |
| **Severity (嚴重程度)** | Medium | API 不一致，不會導致記憶體錯誤但會造成混淆 |
| **Reproducibility (復現難度)** | Low | 容易重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::is_alive`, `Weak::upgrade`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`Weak::is_alive()` 函數僅檢查 `has_dead_flag()`，但沒有檢查 `ref_count == 0`。這導致 `is_alive()` 和 `upgrade()` 的行為不一致。

### 預期行為
- `is_alive()` 應該返回 `false` 當物件沒有強引用時
- `upgrade()` 返回 `None` 時，`is_alive()` 應該也返回 `false`

### 實際行為
1. `is_alive()` 只檢查 `has_dead_flag()`
2. 當 `ref_count == 0` 但 `has_dead_flag()` 未設置時，`is_alive()` 返回 `true`
3. 但 `upgrade()` 會因為 `ref_count == 0` 返回 `None`
4. **使用者困惑**：`is_alive()` 返回 `true` 但 `upgrade()` 返回 `None`

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1638-1645` 的 `is_alive()` 函數中：

```rust
pub fn is_alive(&self) -> bool {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return false;
    };

    // 問題：只檢查 has_dead_flag()，沒有檢查 ref_count
    unsafe { !(*ptr.as_ptr()).has_dead_flag() }
}
```

對比 `Weak::upgrade()` (`ptr.rs:1489-1492`)：
```rust
let current_count = gc_box.ref_count.load(Ordering::Relaxed);
if current_count == 0 {
    return None;  // 這裡會返回 None
}
```

問題：
- `is_alive()` 不檢查 `ref_count == 0` 的情況
- 當物件的強引用全部被 drop 但 DEAD_FLAG 尚未設置時，`is_alive()` 會錯誤地返回 `true`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    // 移除強引用
    drop(gc);
    
    // 這裡 DEAD_FLAG 可能還沒設置
    let is_alive_result = weak.is_alive();
    
    // 嘗試升級
    let upgrade_result = weak.upgrade();
    
    println!("is_alive = {}", is_alive_result);
    println!("upgrade = {:?}", upgrade_result.is_some());
    
    // 預期：兩者應該一致（都返回 false）
    // 實際：is_alive 可能返回 true，但 upgrade 返回 None
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：使用 is_dead_or_unrooted() 檢查（推薦）

```rust
pub fn is_alive(&self) -> bool {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return false;
    };

    // 檢查 dead flag 或 ref_count == 0
    unsafe { !(*ptr.as_ptr()).is_dead_or_unrooted() }
}
```

### 方案 2：文檔化並依賴升級

在文件中說明 `is_alive()` 是不確定的，並建議使用 `upgrade()` 替代。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 API 一致性問題。在傳統 GC 中，Weak ref 的 `is_alive` 和 `upgrade` 通常是一致的。rudo-gc 需要確保這兩個方法的語義一致，以避免使用者困惑。

**Rustacean (Soundness 觀點):**
這不是記憶體安全問題，但是不良的 API 設計。`is_alive()` 和 `upgrade()` 返回不一致的結果會導致邏輯錯誤。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個不一致性：
1. 依賴 `is_alive()` 返回 true 來假設物件有效
2. 實際上 `upgrade()` 會返回 None
3. 可能導致邏輯錯誤而非記憶體錯誤
