# [Bug]: Gc::downgrade() 文件說會 panic 但實際不會

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能會看到文件描述後依賴此行為 |
| **Severity (嚴重程度)** | Medium | 導致文件與實作不一致，可能造成預期外的行為 |
| **Reproducibility (復現難度)** | Very High | 直接檢視程式碼即可發現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>::downgrade()` method
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
根據文件，`downgrade()` 應該在 Gc 為 dead 時 panic：

```rust
/// Create a `Weak<T>` pointer to this allocation.
///
/// # Panics
///
/// Panics if the Gc is dead.
pub fn downgrade(gc: &Self) -> Weak<T> {
```

### 實際行為 (Actual Behavior)
`downgrade()` 實作直接遞增 weak count，沒有檢查 `has_dead_flag()` 或 `dropping_state()`：

```rust
pub fn downgrade(gc: &Self) -> Weak<T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    // Increment the weak count
    // SAFETY: ptr is valid and not null
    unsafe {
        (*gc_box_ptr).inc_weak();  // 沒有任何檢查！
    }
    Weak {
        ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/ptr.rs:1148-1159`

文件與實作不一致。這個問題與 bug47, bug48, bug49 類似，但影響不同的函數：

| 函數 | 文件說 Panic | 實際有檢查 |
|------|-------------|-----------|
| `as_ptr()` (bug47) | ✓ | ✗ |
| `ref_count()` (bug49) | ✓ | ✗ |
| `weak_count()` (bug49) | ✓ | ✗ |
| `downgrade()` (本 bug) | ✓ | ✗ |
| `try_deref()` | N/A | ✓ 檢查兩者 |
| `try_clone()` (bug48) | N/A | ✗ 漏掉 dropping_state |
| `upgrade()` | ✓ | ✓ 檢查 under_construction |

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
    
    drop(gc);
    collect_full();
    
    // 文件說這裡應該 panic，但實際不會
    // 會返回一個指向已回收記憶體的 Weak指標
    // let _ = Gc::downgrade(&gc);  // 未定義行為！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有兩個選項：

1. **移除文件中的 panic 描述**（如果這是預期行為）：
```rust
/// Create a `Weak<T>` pointer to this allocation.
pub fn downgrade(gc: &Self) -> Weak<T> {
```

2. **實作文件中描述的 panic 行為**：
```rust
/// Create a `Weak<T>` pointer to this allocation.
///
/// # Panics
///
/// Panics if the Gc is dead.
pub fn downgrade(gc: &Self) -> Weak<T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::downgrade: Gc is dead"
        );
        (*gc_box_ptr).inc_weak();
    }
    Weak {
        ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 cyclic reference GC 中，`downgrade` 允許在物件仍被引用時創建 weak reference。但如果物件已經死亡（ref count 為 0 且已標記），仍然允許創建 weak reference 可能導致 weak count 不正確，進而影響後續的記憶體回收判斷。

**Rustacean (Soundness 觀點):**
這是一個文件與實作不一致的問題。雖然不會直接造成 UB，但會誤導開發者依賴 panic 來做安全檢查。如果開發者依賴此行為做為安全防線，可能導致預期外的記憶體操作。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個差異，在物件死亡後仍然試圖創建 weak reference，進一步探索記憶體佈局。
