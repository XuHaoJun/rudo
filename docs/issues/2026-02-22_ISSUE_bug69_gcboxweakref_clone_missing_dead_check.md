# [Bug]: GcBoxWeakRef::clone() 缺少 dead_flag / dropping_state 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在物件已死亡或正在 dropping 時 clone GcBoxWeakRef |
| **Severity (嚴重程度)** | Medium | 可能導致為已死亡物件增加 weak count，導致記憶體管理不一致 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::clone()` in `ptr.rs:458-467`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當呼叫 `GcBoxWeakRef::clone()` 時，如果物件已經死亡（`has_dead_flag()` 為 true）或正在被 drop（`dropping_state() != 0`），應該返回失敗或增加失敗的處理。

這與 `Weak::clone()` 的預期行為一致（見 bug64）。

### 實際行為 (Actual Behavior)

目前 `GcBoxWeakRef::clone()` **沒有**檢查：
- `has_dead_flag()`
- `dropping_state()`

直接調用 `inc_weak()` 而不檢查物件狀態，導致可能為已死亡或正在 dropping 的物件增加 weak count。

### 影響範圍

此方法被以下程式碼使用：
- `WeakCrossThreadHandle::clone()` (cross_thread.rs:460-467) - 直接 delegate 給 `self.weak.clone()`

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `ptr.rs:458-467` (`GcBoxWeakRef::clone()`)

對比 `Weak::upgrade()` (ptr.rs:422-456) 有正確的檢查：

```rust
pub(crate) fn upgrade(&self) -> Option<Gc<T>> {
    // ...
    unsafe {
        let gc_box = &*ptr.as_ptr();

        if gc_box.is_under_construction() {  // 有檢查！
            return None;
        }

        if gc_box.has_dead_flag() {  // 有檢查！
            return None;
        }

        if gc_box.dropping_state() != 0 {  // 有檢查！
            return None;
        }
        // ...
    }
}
```

但 `GcBoxWeakRef::clone()` 缺少這些檢查：

```rust
pub(crate) fn clone(&self) -> Self {
    let ptr = self.ptr.load(Ordering::Acquire).as_option().unwrap();
    unsafe {
        (*ptr.as_ptr()).inc_weak();  // 缺少: has_dead_flag() 和 dropping_state() 檢查！
    }
    Self {
        ptr: AtomicNullable::new(ptr),
    }
}
```

這與 bug64 發現的 `Weak::clone()` 缺少檢查的問題類似。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 此 bug 會影響 WeakCrossThreadHandle::clone()
//
// 當 cross-thread weak handle 被 clone 時，
// 如果底層物件已經死亡或正在 dropping，
// GcBoxWeakRef::clone() 會錯誤地增加 weak count
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBoxWeakRef::clone()` 中添加檢查：

```rust
pub(crate) fn clone(&self) -> Self {
    let ptr = self.ptr.load(Ordering::Acquire).as_option().unwrap();
    
    // 新增: 檢查 dead_flag 和 dropping_state
    unsafe {
        let gc_box = &*ptr.as_ptr();
        if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 {
            // Return a null/empty weak reference
            return Self {
                ptr: AtomicNullable::null(),
            };
        }
        (*ptr.as_ptr()).inc_weak();
    }
    Self {
        ptr: AtomicNullable::new(ptr),
    }
}
```

這與 `Weak::upgrade()` 的行為一致，確保在物件已死亡或正在 dropping 時，clone 會返回空的 weak reference。

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

---

## 關聯 Issue

- bug31: Weak::clone TOCTOU - 提及 GcBoxWeakRef::clone 也有類似問題
- bug64: Weak::clone 缺少 dead_flag/dropping_state 檢查 - 類似的問題
- bug68: Gc::as_weak() 缺少 dead_flag/dropping_state 檢查 - 類似的問題

---

## Resolution (2026-02-26)

**Outcome:** Already fixed.

The fix was applied in commit `3ccae47` ("fix: add safety checks to GcBoxWeakRef::clone"). The current `GcBoxWeakRef::clone()` implementation in `ptr.rs` (lines 488-497) correctly checks `has_dead_flag()` and `dropping_state() != 0`, returning a null weak ref when either is true. Behavior matches `Weak::clone()`.
