# [Bug]: Gc::as_ptr() 文件與實作不符 - 文件說會 panic 但實際不會

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能會看到文件描述後依賴此行為 |
| **Severity (嚴重程度)** | Medium | 導致文件與實作不一致，可能造成預期外的行為 |
| **Reproducibility (復現難度)** | Very High | 直接檢視程式碼即可發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>::as_ptr()` method
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
根據文件（line 1085-1087），`as_ptr()` 應該在 Gc 為 dead 時 panic：
```rust
/// # Panics
///
/// Panics if the Gc is dead.
pub fn as_ptr(&self) -> *const T {
```

### 實際行為 (Actual Behavior)
`as_ptr()` 實作（ptr.rs:1088-1093）直接返回指標，沒有檢查 `has_dead_flag()` 或 `dropping_state()`：
```rust
pub fn as_ptr(&self) -> *const T {
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    // SAFETY: ptr is not null (checked in callers), and ptr is valid
    unsafe { std::ptr::addr_of!((*gc_box_ptr).value) }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/ptr.rs:1088-1093`

文件與實作不一致。相較於其他方法：
- `try_deref()` (line 1059): 檢查 `has_dead_flag()` 和 `dropping_state() != 0`
- `Deref::deref()` (line 1286-1288): 檢查兩者並 panic
- `as_ptr()`: 文件說會 panic 但沒有實作檢查

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let ptr = gc.as_ptr();
    
    drop(gc);
    collect_full();
    
    // 文件說這裡應該 panic，但實際不會
    // 會返回已回收記憶體的指標
    let ptr_after_gc = gc.as_ptr();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有兩個選項：

1. **移除文件中的 panic 描述**（如果這是預期行為）：
```rust
/// Get a raw pointer to the data.
pub fn as_ptr(&self) -> *const T {
```

2. **實作文件中描述的 panic 行為**：
```rust
/// Get a raw pointer to the data.
///
/// # Panics
///
/// Panics if the Gc is dead.
pub fn as_ptr(&self) -> *const T {
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    // SAFETY: ptr is not null (checked in callers)
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::as_ptr: Gc is dead"
        );
        std::ptr::addr_of!((*gc_box_ptr).value)
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
文件與實作的不一致會造成 GC API 使用上的困惑。在Chez Scheme中，我們確保所有公開 API 的文件與行為一致，避免造成預期外的記憶體操作。

**Rustacean (Soundness 觀點):**
這是一個文件與實作不一致的問題。雖然不會直接造成 UB，但會誤導開發者依賴錯誤的行為。

**Geohot (Exploit 觀點):**
如果開發者依賴 `as_ptr()` 在 dead 時 panic 來做安全檢查，攻擊者可能利用這個差異進行預期外的記憶體操作。
