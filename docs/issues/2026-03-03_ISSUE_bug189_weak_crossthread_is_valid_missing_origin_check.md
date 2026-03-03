# [Bug]: WeakCrossThreadHandle::is_valid() Missing Origin Thread Check - Inconsistent with resolve()

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確時序：is_valid() 和 resolve() 之间origin thread終止 |
| **Severity (嚴重程度)** | Medium | 可能導致 panic 或不一致行為 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::is_valid()` in `handles/cross_thread.rs:453-455`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`WeakCrossThreadHandle::is_valid()` 應該與 `resolve()` / `try_resolve()` 的行為一致，檢查 origin thread 是否仍然存活。如果 origin thread 已終止，`is_valid()` 應該返回 `false`，與 `try_resolve()` 的行為一致。

### 實際行為 (Actual Behavior)

`WeakCrossThreadHandle::is_valid()` 只檢查 weak ref 是否可升級，**不檢查** origin thread 是否存活：

```rust
// handles/cross_thread.rs:453-455
pub fn is_valid(&self) -> bool {
    self.weak.upgrade().is_some()  // 沒有檢查 origin_tcb.upgrade()!
}
```

但 `resolve()` 會檢查並 panic：

```rust
// handles/cross_thread.rs:472-478
pub fn resolve(&self) -> Option<Gc<T>> {
    if self.origin_tcb.upgrade().is_none() {  // 有檢查！
        panic!(...);
    }
    ...
}
```

`try_resolve()` 也會檢查並返回 `None`：

```rust
// handles/cross_thread.rs:498
self.origin_tcb.upgrade()?;  // 有檢查！
```

### 程式碼位置

`handles/cross_thread.rs` 第 453-455 行 (`WeakCrossThreadHandle::is_valid` 實作)：
```rust
pub fn is_valid(&self) -> bool {
    self.weak.upgrade().is_some()  // <-- 缺少 origin thread 檢查！
}
```

對比 `GcHandle::is_valid()` (lines 100-114) - 它正確地檢查了 root list：
```rust
pub fn is_valid(&self) -> bool {
    if self.handle_id == HandleId::INVALID {
        return false;
    }
    self.origin_tcb.upgrade().map_or_else(
        || { /* 檢查 orphan */ },
        |tcb| { /* 檢查 roots */ },
    )
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**根本原因：**
1. `WeakCrossThreadHandle::is_valid()` 實現不完整 - 只檢查 weak ref 本身的可升級性
2. 缺少對 origin thread 存活的檢查，與 `resolve()` / `try_resolve()` 行為不一致

**Race 條件：**
1. Thread A: 調用 `weak_handle.is_valid()` - 返回 `true`（object 活著）
2. Thread B: Origin thread 終止
3. Thread A: 調用 `weak_handle.resolve()` - **Panic!** (因為 origin 已終止)
   或 `weak_handle.try_resolve()` - 返回 `None`（因為 origin 已終止）

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的時序控制，單執行緒無法穩定重現。

概念驗證：
```rust
// 偽代碼展示不一致行為
let weak = create_weak_cross_thread_handle();
let handle_from_other_thread = send_to_other_thread(weak);

// Other thread:
if handle_from_other_thread.is_valid() {
    // 這裡 origin thread 可能已經終止
    // 調用 resolve() 會 panic，調用 try_resolve() 會返回 None
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `WeakCrossThreadHandle::is_valid()` 加入 origin thread 存活檢查：

```rust
pub fn is_valid(&self) -> bool {
    // 先檢查 origin thread 存活（與 resolve/try_resolve 一致）
    if self.origin_tcb.upgrade().is_none() {
        return false;
    }
    self.weak.upgrade().is_some()
}
```

或者，如果認為 is_valid() 應該是輕量級檢查，則應該在文檔中明確說明此行為差異。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Weak handles 在設計上不需要 root 追蹤（因為不阻止 collection），但跨執行緒的 handle 仍需確保 origin thread 存活的時序一致性。`is_valid()` 與 `resolve()` 的行為不一致會導致 API 使用上的困惑。

**Rustacean (Soundness 觀點):**
這不是嚴格的 UB，但是不一致的 API 行為。從安全角度，`is_valid()` 返回 true 後 `resolve()` 不應該 panic。

**Geohot (Exploit 觀點):**
精確時序攻擊場景：攻擊者可以通過控制執行緒生命週期，在 `is_valid()` 和 `resolve()` 之間造成 race，導致目標程式 panic。

---

## Resolution (2026-03-03)

**Fixed.** `WeakCrossThreadHandle::is_valid()` now checks `origin_tcb.upgrade()` before `weak.upgrade()`, consistent with `resolve()` and `try_resolve()`. Added `test_weak_is_valid_after_origin_thread_terminated` to verify behavior when origin thread has terminated.
