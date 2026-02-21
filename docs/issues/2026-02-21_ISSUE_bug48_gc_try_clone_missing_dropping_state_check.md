# [Bug]: Gc::try_clone 缺少 dropping_state 檢查 - 與 try_deref 行為不一致

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 concurrent GC 或 cyclic reference drop 期間可能發生 |
| **Severity (嚴重程度)** | High | 可能導致 use-after-free 或存取正在 drop 的物件 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序條件（物件正在 drop 時呼叫 try_clone） |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>::try_clone()` method
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`try_clone()` 應該與 `try_deref()` 行為一致 - 當物件正在被 drop (`dropping_state() != 0`) 時，應該返回 `None`。

### 實際行為 (Actual Behavior)
`try_clone()` 只檢查 `has_dead_flag()`，但沒有檢查 `dropping_state() != 0`。

**對比：**

1. `try_deref()` (ptr.rs:1052-1063) - 正確檢查兩者：
```rust
if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
    return None;
}
```

2. `try_clone()` (ptr.rs:1069-1081) - 只檢查 dead_flag：
```rust
if (*gc_box_ptr).has_dead_flag() {
    return None;
}
// 缺少 dropping_state() 檢查！
```

3. `Clone::clone()` (ptr.rs:1295-1318) - 完全沒有檢查：
```rust
fn clone(&self) -> Self {
    let ptr = self.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return Self { ptr: AtomicNullable::null(), _marker: PhantomData };
    }
    // 沒有任何檢查！
    unsafe { (*gc_box_ptr).inc_ref(); }
    // ...
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/ptr.rs:1069-1081`

`try_clone` 函數在檢查物件是否可存取時，漏掉了 `dropping_state() != 0` 的檢查。這導致：

1. 當物件正在 drop 過程中（`dropping_state() == 1` 或 `== 2`），`try_deref` 會正確返回 `None`
2. 但 `try_clone` 會錯誤地返回 `Some(Gc)`，允許存取一個正在被摧毀的物件

此外，`Clone::clone()` 更是完全沒有檢查，這更危險。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

#[derive(Trace)]
struct Container {
    inner: GcCell<Option<Gc<Data>>>,
}

fn main() {
    // 建立 cyclic reference 導致 drop 時需要兩階段處理
    let container = Gc::new_cyclic_weak(|weak_self| {
        Container {
            inner: GcCell::new(Some(Gc::new(Data { value: 42 }))),
        }
    });
    
    let data_ptr: *const Data = {
        let cell = container.inner.borrow();
        cell.as_ref().unwrap().as_ptr()
    };
    
    // 移除 strong reference 觸發 drop
    drop(container);
    
    // 在 drop 過程中調用 try_clone
    // try_deref 會返回 None（正確）
    // 但 try_clone 可能返回 Some（不正確）
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. **修復 `try_clone`** - 添加 `dropping_state() != 0` 檢查：
```rust
pub fn try_clone(gc: &Self) -> Option<Self> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
            return None;
        }
    }
    Some(gc.clone())
}
```

2. **考慮修復 `Clone::clone`** - 添加基本檢查（可選，視為 API 破壞性變更）：
```rust
impl<T: Trace> Clone for Gc<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire);
        if ptr.is_null() {
            return Self {
                ptr: AtomicNullable::null(),
                _marker: PhantomData,
            };
        }

        let gc_box_ptr = ptr.as_ptr();
        
        // 添加檢查以防止 clone 已死或正在 drop 的物件
        unsafe {
            if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
                return Self {
                    ptr: AtomicNullable::null(),
                    _marker: PhantomData,
                };
            }
            (*gc_box_ptr).inc_ref();
        }
        // ...
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 cyclic reference GC 中，物件的 drop 過程需要多個階段。`dropping_state` 是用來防止在 drop 過程中進行 concurrent upgrade 的關鍵機制。漏掉這個檢查會導致在物件正在摧毀時仍然可以取得新的 strong reference，可能造成記憶體損壞。

**Rustacean (Soundness 觀點):**
這是一個 soundness 問題。如果 `try_clone` 在物件正在 drop 時返回 `Some`，呼叫者可能會存取一個已經部分 drop 的物件，導致 undefined behavior。

**Geohot (Exploit 觀點):**
攻擊者可能利用這個時間視窗，在物件 drop 過程中取得一個看似有效的 Gc 指標，進而存取已釋放或部分摧毀的記憶體。
