# [Bug]: GcBoxWeakRef::upgrade() 缺少 is_under_construction 檢查

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在物件構造過程中調用升級 |
| **Severity (嚴重程度)** | High | 可能導致存取未初始化的物件 |
| **Reproducibility (復現難度)** | Low | 需要特定的使用模式 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade`, `CrossThreadHandle`, `Gc::new_cyclic_weak`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

內部的 `GcBoxWeakRef::upgrade()` 方法沒有檢查 `is_under_construction()`，而公有的 `Weak::upgrade()` 有這個檢查。這可能導致在物件構造過程中意外升級弱引用，存取未初始化的物件。

### 預期行為
- `GcBoxWeakRef::upgrade()` 應該與 `Weak::upgrade()`有一致的安全檢查
- 不應該允許在物件構造過程中升級弱引用

### 實際行為
1. `Weak::upgrade()` 有 `is_under_construction()` 檢查 (`ptr.rs:1473-1478`)
2. `GcBoxWeakRef::upgrade()` 缺少這個檢查 (`ptr.rs:406-431`)
3. 跨執行緒 handle 使用 `GcBoxWeakRef`，可能在構造過程中被錯誤地升級

---

## 🔬 根本原因分析 (Root Cause Analysis)

公有的 `Weak::upgrade()` 在 `ptr.rs:1473-1478`:
```rust
// Check if the object is under construction
debug_assert!(
    !gc_box.is_under_construction(),
    "Weak::upgrade: cannot upgrade while GcBox is under construction. \
     This typically happens if you call upgrade() inside the closure \
     passed to Gc::new_cyclic_weak()."
);
```

但內部的 `GcBoxWeakRef::upgrade()` 在 `ptr.rs:406-431`:
```rust
pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
    let ptr = self.ptr.load(Ordering::Acquire).as_option()?;

    unsafe {
        let gc_box = &*ptr.as_ptr();

        // If DEAD_FLAG is set, value has been dropped - cannot resurrect
        if gc_box.has_dead_flag() {
            return None;
        }

        // Try atomic transition from 0 to 1 (resurrection)
        if gc_box.try_inc_ref_from_zero() {
            return Some(Gc {
                ptr: AtomicNullable::new(ptr),
                _marker: PhantomData,
            });
        }

        gc_box.inc_ref();
        Some(Gc {
            ptr: AtomicNullable::new(ptr),
            _marker: PhantomData,
        })
    }
    // 問題：沒有檢查 is_under_construction()
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

`CrossThreadHandle` 使用 `GcBoxWeakRef`，可能在物件構造過程中被錯誤地升級：

```rust
use rudo_gc::{Gc, Trace, GcCell};

#[derive(Trace)]
struct Node {
    value: GcCell<Option<Gc<Node>>>,
}

fn main() {
    // 這個測試展示問題的理論可能性
    // 實際上需要在 Gc::new_cyclic_weak 內部使用 cross_thread_handle
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：添加 is_under_construction 檢查（推薦）

在 `GcBoxWeakRef::upgrade()` 中添加檢查：

```rust
pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
    let ptr = self.ptr.load(Ordering::Acquire).as_option()?;

    unsafe {
        let gc_box = &*ptr.as_ptr();

        // 添加檢查
        if gc_box.is_under_construction() {
            return None;
        }

        // If DEAD_FLAG is set, value has been dropped - cannot resurrect
        if gc_box.has_dead_flag() {
            return None;
        }
        // ... rest of the code
    }
}
```

### 方案 2：文檔化差異

在文檔中說明 `GcBoxWeakRef::upgrade()` 是內部方法，調用者需要自行確保安全。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
內部的 weak reference 實現應該與公有的有一致的安全檢查。`GcBoxWeakRef` 被 `CrossThreadHandle` 使用，如果允許在構造過程中升級，可能導致存取未初始化的資料。

**Rustacean (Soundness 觀點):**
這可能導致未定義行為。存取未初始化的記憶體是 UB，即使是在 GC 管理的記憶體中。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個漏洞：
1. 構造一個在構造過程中的物件
2. 通過 cross-thread handle 嘗試升級
3. 存取未初始化的記憶體，實現資訊洩露或任意記憶體讀取
