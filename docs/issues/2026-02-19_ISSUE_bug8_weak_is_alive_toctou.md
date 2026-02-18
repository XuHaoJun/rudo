# [Bug]: Weak::is_alive() 存在 TOCTOU 競爭條件可能導致 Use-After-Free

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 is_alive() 和 GC 並發執行時觸發 |
| **Severity (嚴重程度)** | Critical | 可能導致 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::is_alive`, `Weak::upgrade`, Weak Reference
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`Weak::is_alive()` 函數存在 TOCTOU (Time-Of-Check-Time-Of-Use) 競爭條件。在加載指標和解引用指標之間，物件可能被 GC 回收，導致 use-after-free。

```rust
pub fn is_alive(&self) -> bool {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return false;
    };

    // 問題：在這裡和上面之間，物件可能被 GC 回收
    unsafe { !(*ptr.as_ptr()).has_dead_flag() }
}
```

### 預期行為
- `is_alive()` 應該安全地檢查物件是否存活
- 不應該發生記憶體錯誤

### 實際行為
1. 載入指標 (`ptr.load`)
2. **GC 可能在此時發生，物件被回收**
3. 解引用指標 (`*ptr.as_ptr()`) → **UAF!**

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1638-1645` 的 `is_alive()` 函數中：

```rust
#[must_use]
pub fn is_alive(&self) -> bool {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return false;
    };

    // SAFETY: The pointer is valid because we have a weak reference
    // 這個註解是錯誤的！
    unsafe { !(*ptr.as_ptr()).has_dead_flag() }
}
```

問題：
1. 指標加載使用 `Acquire` 順序
2. 但在加載和解引用之間沒有同步
3. GC 可以在此時運行並回收物件
4. `has_dead_flag()` 讀取已經釋放的記憶體

相同的問題也存在于其他 Weak 函數：
- `strong_count()` (`ptr.rs:1666-1683`)
- `weak_count()` (`ptr.rs:1687-1702`)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, collect_full};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    // 使用一個 flag 來同步時序
    let is_alive_called = Arc::new(AtomicBool::new(false));
    let is_alive_called_clone = is_alive_called.clone();
    
    let handle = thread::spawn(move || {
        // 等待 drop 發生
        while !is_alive_called_clone.load(Ordering::Relaxed) {
            thread::yield();
        }
        
        // 這裡調用 is_alive 可能會 UAF
        let alive = weak.is_alive();
        println!("is_alive = {}", alive);
    });
    
    drop(gc);
    collect_full();
    
    // 通知另一個執行緒調用 is_alive
    is_alive_called.store(true, Ordering::Relaxed);
    
    handle.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：使用更強的原子操作（推薦）

在讀取指標後，使用原子操作確保 GC 不會干擾：

```rust
pub fn is_alive(&self) -> bool {
    let ptr = self.ptr.load(Ordering::Acquire);
    let Some(ptr) = ptr.as_option() else {
        return false;
    };

    // 使用 Acquire 語義確保讀取 has_dead_flag 之前的所有寫入都可見
    // 同時防止 GC 在此期間回收物件
    let dead_flag = (*ptr.as_ptr()).ref_count()
        .load(Ordering::Acquire);
    
    // 如果 ref_count 為 0 或 dead_flag 設置，則物件已死亡
    // 但這種方法也有問題，因為 ref_count 可能是最後一個強引用
    
    // 更好的方法：
    // 嘗試獲取一個臨時的強引用來"保護"物件
    self.upgrade().is_some()
}
```

### 方案 2：在 is_alive 中添加記憶體有效性檢查

```rust
pub fn is_alive(&self) -> bool {
    let Some(ptr) = self.ptr.load(Ordering::Acquire).as_option() else {
        return false;
    };

    unsafe {
        // 檢查記憶體是否仍然映射
        let ptr_addr = ptr.as_ptr() as usize;
        
        // 嘗試讀取一個字節來檢查記憶體是否有效
        // 這是一個 hack，但比 UAF 好
        let result = std::ptr::read_volatile(ptr_addr as *const u8);
        
        // 如果讀取成功，檢查 dead flag
        !(*ptr.as_ptr()).has_dead_flag()
    }
}
```

### 方案 3：文檔化並依賴升級

在文檔中說明 `is_alive()` 是不安全的，並建議使用 `upgrade()` 替代：

```rust
/// 檢查 Weak 引用是否仍然有效。
///
/// # 警告
///
/// 此方法在 GC 並發運行時可能導致 use-after-free。
/// 請使用 `upgrade().is_some()` 替代。
///
/// # Safety
///
/// 調用者必須確保在調用期間不會發生 GC。
#[must_use]
pub unsafe fn is_alive_unchecked(&self) -> bool {
    // ...
}

// 安全的替代方案
pub fn is_alive(&self) -> bool {
    self.upgrade().is_some()
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Weak 引用在 GC 環境中的實現需要特別小心。在傳統的 GC 實現中，通常通過在物件頭部維護額外的元數據來追蹤物件狀態，而不是通過指標加載。rudo-gc 需要確保 Weak 引用在各種並發場景下都是安全的。

**Rustacean (Soundness 觀點):**
這是明確的未定義行為。解引用已釋放的記憶體是 UB，無論是否透過 Weak 引用。必須修復以確保記憶體安全。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過：
1. 構造精確時序的 is_alive() 調用
2. 控制 GC 時機
3. 洩露記憶體佈局資訊
4. 可能實現任意記憶體讀取（如果配合其他漏洞）

