# [Bug]: WeakCrossThreadHandle::is_valid() Missing is_allocated Check - Inconsistent with upgrade()

**Status:** Invalid
**Tags:** Not Reproduced

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 lazy sweep 回收並重用 slot 後調用 is_valid() 時觸發 |
| **Severity (嚴重程度)** | Medium | API 行為不一致，is_valid() 返回 true 但 upgrade() 返回 None |
| **Reproducibility (復現難度)** | Medium | 需要觸發 lazy sweep 後調用 is_valid() |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::is_valid()`, `handles/cross_thread.rs:635-640`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`WeakCrossThreadHandle::is_valid()` 應該與 `Weak::is_alive()` 行為一致：
- `Weak::is_alive()` 內部調用 `upgrade()` 來檢查，因此會檢查 `is_allocated`
- 這樣可以避免 TOCTOU：is_valid() 返回 true 但升級失敗

### 實際行為 (Actual Behavior)

`WeakCrossThreadHandle::is_valid()` 使用 `weak.is_live()`，該函數**不檢查** `is_allocated`：
- `GcBoxWeakRef::is_live()` 只檢查：null、address validity、has_dead_flag
- 但 `GcBoxWeakRef::try_upgrade()` 還會額外檢查 `is_allocated`

這導致行為不一致：
1. `WeakCrossThreadHandle::is_valid()` 返回 `true`
2. 但 `WeakCrossThreadHandle::resolve()` / `try_resolve()` 返回 `None`（因為 slot 已被 lazy sweep 回收並重用）

**代碼位置：** `handles/cross_thread.rs` 第 635-640 行

```rust
pub fn is_valid(&self) -> bool {
    if self.origin_tcb.upgrade().is_none() {
        return false;
    }
    self.weak.is_live()  // <-- 不檢查 is_allocated！
}
```

### 對比：Regular Weak::is_alive() 的正確實現

```rust
// ptr.rs:2395-2400
pub fn is_alive(&self) -> bool {
    // Delegate to upgrade() to avoid TOCTOU
    self.upgrade().is_some()  // <-- 調用 upgrade()，會檢查 is_allocated
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `GcBoxWeakRef::is_live()` 和 `GcBoxWeakRef::try_upgrade()` 的檢查不一致：

1. `is_live()` (ptr.rs:790-813) 檢查：
   - null pointer
   - address alignment & validity
   - is_gc_box_pointer_valid
   - **沒有檢查 is_allocated**

2. `try_upgrade()` (ptr.rs:816-900) 檢查：
   - 上述所有項目
   - **is_under_construction**
   - **is_dead_or_unrooted**
   - **dropping_state**
   - **is_allocated** ← 關鍵差異！

當 lazy sweep 回收 slot 並重用後：
- `is_live()` 返回 true（因為 dead_flag 未設置）
- `upgrade()` 返回 None（因為 is_allocated 檢查失敗）

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, collect_full};
use std::thread;
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // Create Gc and weak cross-thread handle
    let gc = Gc::new(Data { value: 42 });
    let weak = gc.weak_cross_thread_handle();
    
    // Drop the strong reference
    drop(gc);
    
    // Trigger full collection to ensure object is dead
    collect_full();
    
    // At this point, the slot may be reallocated by subsequent allocations
    // is_valid() might still return true (false positive)
    let is_valid_result = weak.is_valid();
    println!("is_valid(): {}", is_valid_result);
    
    // But try_resolve() should return None (correct behavior)
    let try_resolve_result = weak.try_resolve();
    println!("try_resolve(): {:?}", try_resolve_result);
    
    // This demonstrates the inconsistency:
    // is_valid() returns true while upgrade would fail
    if is_valid_result && try_resolve_result.is_none() {
        println!("BUG CONFIRMED: is_valid() true but upgrade() None!");
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

有兩種修復方案：

**方案 1：参考 Weak::is_alive()，調用 upgrade() 避免 TOCTOU**
```rust
pub fn is_valid(&self) -> bool {
    if self.origin_tcb.upgrade().is_none() {
        return false;
    }
    // 使用 try_upgrade() 來避免 TOCTOU，並確保與 resolve() 行為一致
    self.weak.try_upgrade().is_some()
}
```

**方案 2：只添加 is_allocated 檢查（更輕量）**
```rust
pub fn is_valid(&self) -> bool {
    if self.origin_tcb.upgrade().is_none() {
        return false;
    }
    
    let ptr = match self.weak.as_ptr() {
        Some(ptr) => ptr,
        None => return false,
    };
    
    // 先檢查 is_live（輕量）
    if !self.weak.is_live() {
        return false;
    }
    
    // 額外檢查 is_allocated（與 upgrade() 一致）
    if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return false;
        }
    }
    
    true
}
```

**推薦方案 1**：與 `Weak::is_alive()` 的實現模式一致，且更簡潔。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 這是 lazy sweep 引入後的經典問題：slot 回收後指標仍然有效（通過 address validity 檢查），但對象已不存在
- `is_live()` 是 lightweight check，但不够嚴重；對於 `is_valid()` 這種需要與 `upgrade()` 保持一致的 API，應該使用完整檢查
- 類似的問題在incremental marking 中也出現過（slot 在 marking 過程中被回收）

**Rustacean (Soundness 觀點):**
- 這不是 soundness 問題（不會導致 UAF 或 memory corruption）
- 是 API 行為不一致的問題：`is_valid()` 和 `upgrade()` 應該保證 is_valid() ⇒ upgrade().is_some()
- 這種不一致會導致用戶邏輯錯誤

**Geohot (Exploit 攻擊觀點):**
- 理論上可以利用這個不一致：
  - 攻擊者讓 `is_valid()` 返回 true
  - 但實際調用 `resolve()` 時失敗
  - 可能導致錯誤的錯誤處理邏輯
- 實際利用難度較高

---

## 備註

此 bug 與以下現有 issue 相關但不同：
- bug122: WeakCrossThreadHandle::is_valid() 在 origin thread 終止後的行為
- bug313: GcHandle::is_valid() TOCTOU race

當前 bug 是關於 `is_valid()` 缺少 `is_allocated` 檢查，導致與 `upgrade()` 行為不一致。

---

## Resolution (2026-03-21)

**Outcome:** Invalid — issue premise is incorrect in the current codebase.

The issue claimed `GcBoxWeakRef::is_live()` does not check `is_allocated`. However, the current implementation at `ptr.rs:860–865` **already includes** the `is_allocated` check:

```rust
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return false;
    }
}
```

`WeakCrossThreadHandle::is_valid()` calls `self.weak.is_live()`, which performs all necessary checks including `is_allocated`. The inconsistency described in the issue does not exist. All 28 cross-thread handle tests pass.
