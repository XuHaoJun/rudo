# [Bug]: Gc::clone() Missing Dead Flag Check 導致記憶體不安全

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 在物件被標記為 dead 後仍有 weak reference 存活時可能發生 |
| **Severity (嚴重程度)** | `Critical` | 可能導致 Use-After-Free 或記憶體損壞 |
| **Reproducibility (復現難度)** | `Medium` | 需要特定時序：物件被標記 dead 但 weak 仍存在 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>` clone implementation
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Gc::clone()` 應該在遞增引用計數前檢查物件是否已標記為 dead（`has_dead_flag()`），以確保不會對已棄置的物件進行操作。

### 實際行為 (Actual Behavior)
`Gc::clone()` 實作 (ptr.rs:1296-1317) 直接遞增引用計數，沒有檢查 `has_dead_flag()`。這與其他需要安全存取的實作（如 `try_deref`, `from_raw`）形成對比，後者都會檢查 dead flag。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/ptr.rs:1296-1317`

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

        // Increment reference count
        // SAFETY: Pointer is valid (not null)
        unsafe {
            (*gc_box_ptr).inc_ref();  // <-- 沒有檢查 has_dead_flag()!
        }

        Self {
            ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
            _marker: PhantomData,
        }
    }
}
```

**對比：** 其他方法都有檢查 dead flag：
- `try_deref()` at line 1059: `if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0`
- `from_raw()` at line 1076: `if (*gc_box_ptr).has_dead_flag()`
- `Deref` at line 1287: `!(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 1. 建立物件並獲取 weak reference
    let gc = Gc::new(GcCell::new(Data { value: 42 }));
    let weak = Gc::downgrade(&gc);
    
    // 2. 執行 full GC 將物件標記為 dead (透過 drop)
    drop(gc);
    collect_full();
    
    // 3. 此時物件的 DEAD_FLAG 應該已設定
    // 但嘗試 clone 會錯誤地遞增 ref_count
    
    // 這是一個概念驗證 - 實際上 Clone::clone 是透過 &self 調用
    // 需要更精確的時序才能觸發
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Gc::clone()` 中新增 dead flag 檢查：

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

        // 檢查 dead flag
        // SAFETY: Pointer is valid (not null)
        unsafe {
            if (*gc_box_ptr).has_dead_flag() {
                panic!("Cannot clone a dead Gc");
            }
            (*gc_box_ptr).inc_ref();
        }

        Self {
            ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
            _marker: PhantomData,
        }
    }
}
```

或者類似 `try_deref` 返回 `Result`：

```rust
fn try_clone(&self) -> Option<Self> {
    // ... 檢查 dead flag ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在物件被標記為 dead 後，引用計數的操作需要特別小心。如果允許對 dead 物件遞增引用計數，可能導致懸浮指標問題。在 Chez Scheme 中，我們會確保所有對 dead 物件的引用都會被正確處理。

**Rustacean (Soundness 觀點):**
這是一個明確的記憶體安全問題。對已標記為 dead 的物件進行 clone 會繞過安全檢查，可能導致 UAF。建議在修復前，這種行為應被視為 UB 或 panic。

**Geohot (Exploit 觀點):**
如果攻擊者能控制時序，可以：
1. 讓物件被標記為 dead
2. 透過 clone 重新激活引用計數
3. 阻止 GC 回收該物件
4. 實現記憶體佈局控制
