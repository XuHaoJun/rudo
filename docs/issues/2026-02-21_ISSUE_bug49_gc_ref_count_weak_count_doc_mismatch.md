# [Bug]: Gc::ref_count() 與 Gc::weak_count() 文件與實作不符 - 文件說會 panic 但實際不會

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能會看到文件描述後依賴此行為 |
| **Severity (嚴重程度)** | Medium | 導致文件與實作不一致，可能造成預期外的行為 |
| **Reproducibility (復現難度)** | Very High | 直接檢視程式碼即可發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>::ref_count()` method, `Gc<T>::weak_count()` method
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
根據文件，`ref_count()` 和 `weak_count()` 應該在 Gc 為 dead 時 panic：

```rust
/// Get the current reference count.
///
/// # Panics
///
/// Panics if the Gc is dead.
pub fn ref_count(gc: &Self) -> NonZeroUsize {
```

```rust
/// Get the current weak reference count.
///
/// # Panics
///
/// Panics if the Gc is dead.
pub fn weak_count(gc: &Self) -> usize {
```

### 實際行為 (Actual Behavior)
`ref_count()` 和 `weak_count()` 實作直接返回計數，沒有檢查 `has_dead_flag()` 或 `dropping_state()`：

```rust
pub fn ref_count(gc: &Self) -> NonZeroUsize {
    let ptr = gc.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    // SAFETY: ptr is not null (checked in callers)
    unsafe { (*gc_box_ptr).ref_count() }
}

pub fn weak_count(gc: &Self) -> usize {
    let ptr = gc.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    // SAFETY: ptr is not null (checked in callers)
    unsafe { (*gc_box_ptr).weak_count() }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/ptr.rs:1110-1127`

文件與實作不一致。相較於其他方法：
- `try_deref()` (line 1059): 檢查 `has_dead_flag()` 和 `dropping_state() != 0`
- `Deref::deref()` (line 1286-1288): 檢查兩者並 panic
- `as_ptr()` (bug47): 文件說會 panic 但沒有實作檢查
- `ref_count()`: 文件說會 panic 但沒有實作檢查
- `weak_count()`: 文件說會 panic 但沒有實作檢查

這個問題與 bug47 類似，但影響不同的函數。

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
    
    drop(gc);
    collect_full();
    
    // 文件說這裡應該 panic，但實際不會
    // 會返回已回收記憶體的計數或許導致 UB
    // let _ = Gc::ref_count(&gc);  // 未定義行為！
    // let _ = Gc::weak_count(&gc);  // 未定義行為！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有兩個選項：

1. **移除文件中的 panic 描述**（如果這是預期行為）：
```rust
/// Get the current reference count.
pub fn ref_count(gc: &Self) -> NonZeroUsize {
```

2. **實作文件中描述的 panic 行為**：
```rust
/// Get the current reference count.
///
/// # Panics
///
/// Panics if the Gc is dead.
pub fn ref_count(gc: &Self) -> NonZeroUsize {
    let ptr = gc.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::ref_count: Gc is dead"
        );
        (*gc_box_ptr).ref_count()
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
文件與實作的不一致會造成 GC API 使用上的困惑。在Chez Scheme中，我們確保所有公開 API 的文件與行為一致，避免造成預期外的記憶體操作。

**Rustacean (Soundness 觀點):**
這是一個文件與實作不一致的問題。雖然不會直接造成 UB，但會誤導開發者依賴錯誤的行為。如果開發者依賴 panic 來做安全檢查，可能導致未定義行為。

**Geohot (Exploit 攻擊觀點):**
如果開發者依賴 `ref_count()` 或 `weak_count()` 在 dead 時 panic 來做安全檢查，攻擊者可能利用這個差異進行預期外的記憶體操作。
