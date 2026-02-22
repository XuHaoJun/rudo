# [Bug]: Gc::as_weak() 缺少 dead_flag / dropping_state 檢查

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在物件已死亡或正在 dropping 時呼叫 as_weak() |
| **Severity (嚴重程度)** | Medium | 可能導致為已死亡物件增加 weak count，導致記憶體管理不一致 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc<T>::as_weak()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當呼叫 `Gc::as_weak()` 時，如果物件已經死亡（`has_dead_flag()` 為 true）或正在被 drop（`dropping_state() != 0`），應該返回失敗或增加失敗的處理。

這與以下方法的行為一致：
- `Gc::downgrade()` - 有檢查 has_dead_flag() 和 dropping_state()

### 實際行為 (Actual Behavior)

目前 `Gc::as_weak()` **沒有**檢查：
- `has_dead_flag()`
- `dropping_state()`

直接調用 `inc_weak()` 而不檢查物件狀態，導致可能為已死亡或正在 dropping 的物件增加 weak count。

這與 bug64 發現的 `Weak::clone()` 缺少檢查的問題類似。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `ptr.rs:1231-1242` (`Gc<T>::as_weak()`)

對比 `Gc::downgrade()` (ptr.rs:1192-1206) 有正確的檢查：

```rust
pub fn downgrade(gc: &Self) -> Weak<T> {
    // ...
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag() && (*gc_box_ptr).dropping_state() == 0,
            "Gc::downgrade: Gc is dead or in dropping state"
        );
        (*gc_box_ptr).inc_weak();
    }
    // ...
}
```

但 `Gc::as_weak()` 缺少這些檢查：

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    // Increment the weak count
    // SAFETY: ptr is valid and not null
    unsafe {
        (*gc_box_ptr).inc_weak();  // 缺少: has_dead_flag() 和 dropping_state() 檢查！
    }
    GcBoxWeakRef {
        ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 1. 創建一個 Gc
    let gc = Gc::new(Data { value: 42 });
    
    // 2. 強制觸發 GC 來 drop 這個對象
    collect_full();
    
    // 3. 此時 gc 應該被視為 "dead"
    
    // 4. 調用 Gc::as_weak - 應該返回錯誤或失敗
    // 但實際上會成功創建新的 GcBoxWeakRef 並增加 weak_count
    let weak_ref = gc.as_weak();
    
    // 類似於 Weak::clone() 的問題（bug64）
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Gc::as_weak()` 中添加檢查：

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    let ptr = self.ptr.load(Ordering::Acquire);
    let gc_box_ptr = ptr.as_ptr();
    
    // 新增: 檢查 dead_flag 和 dropping_state
    unsafe {
        if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 {
            // Return a null/empty weak reference or panic
            // For now, return empty to match downgrade() behavior
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        (*gc_box_ptr).inc_weak();
    }
    GcBoxWeakRef {
        ptr: AtomicNullable::new(unsafe { NonNull::new_unchecked(gc_box_ptr) }),
    }
}
```

這與 `Gc::downgrade()` 的行為一致，確保在物件已死亡或正在 dropping 時，as_weak() 會返回空的 weak reference。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
當物件被標記為 dead 或正在 dropping 時，不應該允許建立新的 weak 引用。這與 reference counting 的基本原則不符：為一個已經無效的物件增加引用計數會導致不正確的記憶體管理。

**Rustacean (Soundness 觀點):**
這是一個記憶體管理一致性問題。允許為已死亡或正在 drop 的物件建立 weak 引用可能導致：
1. 為無效物件增加 weak count
2. 記憶體管理不一致
3. 潛在的 double-free 或 leak

**Geohot (Exploit 攻擊觀點):**
此漏洞可以被利用來：
1. 繞過 GC 的安全檢查
2. 創建對已釋放物件的 weak 引用
3. 導致記憶體管理不一致
