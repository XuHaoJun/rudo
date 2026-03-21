# [Bug]: WeakCrossThreadHandle::is_valid 不支持 orphan handle，與 GcHandle::is_valid 行為不一致

**Status:** Invalid
**Tags:** Not Reproduced

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 origin 執行緒終止後調用 is_valid() 時觸發 |
| **Severity (嚴重程度)** | Low | API 行為不一致，可能導致錯誤的判斷 |
| **Reproducibility (復現難度)** | Low | 容易重現 - 只需建立 handle，終止執行緒，然後調用 is_valid() |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::is_valid()` in `handles/cross_thread.rs:635-640`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`WeakCrossThreadHandle::is_valid()` 應該與 `GcHandle::is_valid()` 行為一致：
- 當 origin 執行緒終止後（變成 orphan handle），`is_valid()` 仍應能正確判斷 handle 是否有效
- `GcHandle::is_valid()` 會先檢查 orphan roots table，如果 handle 在 orphan table 中則返回 true

### 實際行為 (Actual Behavior)

`WeakCrossThreadHandle::is_valid()` 直接檢查 `origin_tcb.upgrade().is_none()`：
- 如果 origin 執行緒已終止，立即返回 `false`
- 不會檢查 weak reference 本身是否仍然有效

**代碼位置：** `handles/cross_thread.rs` 第 635-640 行

```rust
pub fn is_valid(&self) -> bool {
    if self.origin_tcb.upgrade().is_none() {
        return false;  // 直接返回 false，不檢查 weak ref 本身
    }
    self.weak.is_live()
}
```

對比 `GcHandle::is_valid()` (`handles/cross_thread.rs` 第 100-115 行)：

```rust
pub fn is_valid(&self) -> bool {
    if self.handle_id == HandleId::INVALID {
        return false;
    }
    // 先檢查 orphan roots
    let orphan = heap::lock_orphan_roots();
    if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
        return true;  // orphan handle 仍有效
    }
    drop(orphan);
    // ... 檢查 TCB roots
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`WeakCrossThreadHandle::is_valid()` 缺少對 orphan handle 的支持。當 origin 執行緒終止後：
1. `origin_tcb.upgrade()` 返回 `None`
2. `is_valid()` 立即返回 `false`
3. 不會調用 `self.weak.is_live()` 來檢查 weak reference 本身是否仍然有效

這與 `GcHandle::is_valid()` 的行為不一致，後者會先檢查 orphan roots table。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc: Gc<Data> = Gc::new(Data { value: 42 });
    let weak = gc.downgrade_to_weak_cross_thread();
    
    let origin_thread = thread::current().id();
    
    // 在新執行緒中建立 handle
    let handle = weak;
    
    // 在 origin 執行緒終止後檢查 is_valid
    thread::spawn(move || {
        // 這裡 origin 執行緒已經終止
        let is_valid = handle.is_valid();
        // 預期: 如果 weak ref 仍然有效，應返回 true
        // 實際: 返回 false（因為 origin 執行緒已終止）
    }).join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `WeakCrossThreadHandle::is_valid()` 以支持 orphan handle：

```rust
pub fn is_valid(&self) -> bool {
    // 檢查 weak reference 本身是否有效（無論 origin 是否存活）
    self.weak.is_live()
}
```

或者，更嚴格的做法是先檢查 weak ref，再檢查 origin：

```rust
pub fn is_valid(&self) -> bool {
    // 先檢查 weak reference 本身是否有效
    if !self.weak.is_live() {
        return false;
    }
    // 如果 origin 執行緒已終止，只要 weak ref 有效就返回 true
    if self.origin_tcb.upgrade().is_none() {
        return true;
    }
    true
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Weak reference 的有效性不應該依賴於 origin 執行緒的存活狀態。weak ref 本身是一個獨立的引用計數機制，與執行緒生命週期無關。當 origin 執行緒終止後，weak ref 應該仍然可以獨立地檢查其有效性。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題，而是 API 一致性問題。`WeakCrossThreadHandle::is_valid()` 應該與 `GcHandle::is_valid()` 的行為一致，特別是在 orphan handle 的處理上。

**Geohot (Exploit 觀點):**
這不會導致安全問題，但會造成可用性問題。攻擊者無法利用這個問題，但可能導致使用者對 weak ref 有效性的錯誤判斷。

---

## Resolution (2026-03-21)

**Outcome:** Invalid — duplicate of bug #324. Same root-cause misanalysis.

`WeakCrossThreadHandle::is_valid()` correctly returns `false` when the origin thread has terminated because neither `resolve()` nor `try_resolve()` can succeed in that state. The comparison to `GcHandle::is_valid()` is incorrect: `GcHandle` is a strong root with orphan-table migration; `WeakCrossThreadHandle` is a weak ref with no such path. See bug #324 resolution for full details.

No code change required.
