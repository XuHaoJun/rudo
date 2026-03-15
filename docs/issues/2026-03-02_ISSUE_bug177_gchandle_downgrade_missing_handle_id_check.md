# [Bug]: GcHandle::downgrade 缺少 handle_id 有效性檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要對已註銷的 GcHandle 調用 downgrade |
| **Severity (嚴重程度)** | Medium | 可能導致錯誤的 weak_count 增長 |
| **Reproducibility (復現難度)** | Low | 簡單的程式邏輯錯誤 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade` (`handles/cross_thread.rs:290-306`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為

`GcHandle::downgrade` 應該在執行操作前檢查 `handle_id` 是否有效（不等於 `HandleId::INVALID`），與同一類型中其他方法（如 `clone`、`resolve`、`try_resolve`）的行為一致。

### 實際行為

`GcHandle::downgrade` 未檢查 `handle_id` 是否有效，直接存取指標並調用 `inc_weak()`。這與同一文件中其他方法不一致：

| 方法 | handle_id 檢查 |
| :--- | :--- |
| `GcHandle::resolve` | ✅ 檢查並 panic |
| `GcHandle::clone` | ✅ 檢查並 panic |
| `GcHandle::try_resolve` | ✅ 檢查並返回 None |
| `GcHandle::downgrade` | ❌ **未檢查** |

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/cross_thread.rs:290-306`，`GcHandle::downgrade` 函數缺少對 `handle_id` 有效性的檢查：

```rust
// handles/cross_thread.rs:290-306
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    // BUG: 缺少 handle_id != HandleId::INVALID 檢查!
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle"
        );
        gc_box.inc_weak();
    }
    // ...
}
```

對比 `GcHandle::clone` (`handles/cross_thread.rs:312-314`)：
```rust
fn clone(&self) -> Self {
    // ✅ 正確：檢查 handle_id
    if self.handle_id == HandleId::INVALID {
        panic!("cannot clone an unregistered GcHandle");
    }
    // ...
}
```

對比 `GcHandle::resolve` (`handles/cross_thread.rs:166-169`)：
```rust
pub fn resolve(&self) -> Gc<T> {
    // ✅ 正確：檢查 handle_id
    assert!(
        self.handle_id != HandleId::INVALID,
        "GcHandle::resolve: handle has been unregistered"
    );
    // ...
}
```

對比 `GcHandle::try_resolve` (`handles/cross_thread.rs:241-243`)：
```rust
pub fn try_resolve(&self) -> Option<Gc<T>> {
    // ✅ 正確：檢查 handle_id
    if self.handle_id == HandleId::INVALID {
        return None;
    }
    // ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc: Gc<Data> = Gc::new(Data { value: 42 });
    let mut handle = gc.cross_thread_handle();
    
    // Unregister the handle
    handle.unregister();
    
    // BUG: 這應該 panic 或返回錯誤，但目前會執行 inc_weak
    let _weak = handle.downgrade();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

在 `handles/cross_thread.rs:290` 開頭添加 `handle_id` 檢查：

```rust
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    // 添加檢查：與 clone/resolve/try_resolve 保持一致
    assert!(
        self.handle_id != HandleId::INVALID,
        "GcHandle::downgrade: cannot downgrade an unregistered GcHandle"
    );
    
    unsafe {
        // ... existing code
    }
}
```

或者返回 Result：

```rust
pub fn downgrade(&self) -> Result<WeakCrossThreadHandle<T>, HandleError> {
    if self.handle_id == HandleId::INVALID {
        return Err(HandleError::Unregistered);
    }
    // ... existing code
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
如果對已註銷的 handle 執行 downgrade，會錯誤地增加 weak_count，影響循環引用垃圾回收的正確性。當物件應該被回收時，錯誤的 weak_count 可能阻止回收。

**Rustacean (Soundness 觀點):**
這是 API 一致性問題。雖然不會導致嚴重的記憶體不安全，但與同類型其他方法行為不一致，可能導致使用者困惑。

**Geohot (Exploit 攻擊觀點):**
在極端情況下，錯誤的 weak_count 可能被利用來影響 GC 回收行為，但目前看來難以利用。

---

## Resolution (2026-03-03)

**Fixed.** `GcHandle::downgrade` in `handles/cross_thread.rs` already includes the `handle_id != HandleId::INVALID` check (lines 292–295), consistent with `clone`, `resolve`, and `try_resolve`. Added regression test `test_downgrade_unregistered_handle_panics` in `tests/cross_thread_handle.rs`.
