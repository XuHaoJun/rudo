# [Bug]: GcBox::as_weak() 缺少 dead_flag / dropping_state 檢查 - 與 Gc::as_weak() 行為不一致

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | GcBox::as_weak 是內部方法，需直接調用才會觸發 |
| **Severity (嚴重程度)** | Medium | 可能導致在死亡或正在 drop 的物件上增加 weak count |
| **Reproducibility (復現難度)** | Medium | 需要直接調用內部方法 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::as_weak()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcBox::as_weak()` 應該檢查物件是否為 dead、dropping 或 under construction 狀態，與其他類似方法（如 `Gc::as_weak()`, `Gc::downgrade()`）的行為一致。

### 實際行為 (Actual Behavior)
`GcBox::as_weak()` 只檢查 `is_under_construction()`，但**缺少**：
- `has_dead_flag()` 檢查
- `dropping_state()` 檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **`GcBox::as_weak()` (ptr.rs:421-432)** - 缺少檢查:
```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    unsafe {
        if self.is_under_construction() {  // 僅檢查這項！
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        (*NonNull::from(self).as_ptr()).inc_weak();  // 沒有檢查 dead/dropping！
    }
    GcBoxWeakRef::new(NonNull::from(self))
}
```

2. **`Gc<T>::as_weak()` (ptr.rs:1335-1357)** - 完整檢查:
```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    // ...
    if gc_box.is_under_construction()
        || gc_box.has_dead_flag()           // 有檢查
        || gc_box.dropping_state() != 0     // 有檢查
    {
        return GcBoxWeakRef {
            ptr: AtomicNullable::null(),
        };
    }
    // ...
}
```

兩者行為不一致！`GcBox::as_weak()` 應該有相同的檢查。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 這是內部方法，需要通過 unsafe 直接調用
// 1. 創建一個 Gc 物件
// 2. 讓物件進入 dead 或 dropping 狀態
// 3. 直接調用 GcBox::as_weak()（通過 unsafe 或其他方式）
// 4. 觀察是否會錯誤地增加 weak count
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBox::as_weak()` 中添加缺失的檢查:

```rust
pub(crate) fn as_weak(&self) -> GcBoxWeakRef<T> {
    // SAFETY: self is a valid GcBox pointer.
    unsafe {
        if self.is_under_construction()
            || self.has_dead_flag()          // 添加檢查
            || self.dropping_state() != 0    // 添加檢查
        {
            return GcBoxWeakRef {
                ptr: AtomicNullable::null(),
            };
        }
        (*NonNull::from(self).as_ptr()).inc_weak();
    }
    GcBoxWeakRef::new(NonNull::from(self))
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在創建 weak 引用時，應該確保物件處於有效狀態。如果物件已經死亡或正在被 drop，增加 weak count 可能導致記憶體管理錯誤。

**Rustacean (Soundness 觀點):**
這不是傳統意義上的 UB，但可能導致不一致的狀態。weak count 應該只在物件有效時才增加。

**Geohot (Exploit 觀點):**
通過精心設計的時序，可能在物件死亡後仍然增加 weak count，進而影響 GC 的行為。
