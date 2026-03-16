# [Bug]: GcHandle::resolve/try_resolve Post-Increment Check 缺少 is_under_construction 檢查

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要多執行緒交錯執行，且時間窗口很小 |
| **Severity (嚴重程度)** | Medium | 與 bug200 互補，但影響相對較小（物件正在建構中時不太可能有 handle） |
| **Reproducibility (復現難度)** | High | 需要精確的執行緒調度，難以穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve_impl()`, `GcHandle::try_resolve_impl()`
- **File:** `crates/rudo-gc/src/handles/cross_thread.rs`
- **Lines:** 226, 313
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`GcHandle::resolve_impl()` 與 `GcHandle::try_resolve_impl()` 在 `inc_ref()` 之後的 post-increment safety check 缺少 `is_under_construction()` 檢查。

### 預期行為 (Expected Behavior)

Post-increment check 應該檢查所有三個條件：
- `dropping_state() != 0`
- `has_dead_flag()`
- `is_under_construction()`

與 `Weak::try_upgrade()` (ptr.rs:2321-2330) 的 post-CAS check 保持一致。

### 實際行為 (Actual Behavior)

`GcHandle::resolve_impl()` (line 226) 和 `try_resolve_impl()` (line 313) 只檢查：
- `dropping_state() != 0`
- `has_dead_flag()`

**缺少：`is_under_construction()`**

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cross_thread.rs` 中，post-increment check 的實作：

```rust
// resolve_impl:226 - 缺少 is_under_construction!
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(self.ptr.as_ptr());
    panic!("GcHandle::resolve: object was dropped after inc_ref (TOCTOU race)");
}

// try_resolve_impl:313 - 缺少 is_under_construction!
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(self.ptr.as_ptr());
    return None;
}
```

但 `Weak::try_upgrade` (ptr.rs:2321-2330) 正確地包含所有三個檢查：

```rust
if gc_box.dropping_state() != 0
    || gc_box.has_dead_flag()
    || gc_box.is_under_construction()  // <-- 這個檢查存在!
{
    GcBox::undo_inc_ref(ptr.as_ptr());
    return None;
}
```

**bug200 修復了 post-check 缺少 dropping_state 和 dead_flag 的問題，但遺漏了 is_under_construction。**

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要多執行緒環境才能觸發，單執行緒測試無法可靠復現。

理論上的 PoC：
1. 建立 GcBox 並獲取 GcHandle
2. 在另一執行緒中對 GcBox 呼叫最後的 dec_ref()，觸發 dropping
3. 在精確的時間窗口內從原執行緒調用 resolve()
4. 驗證是否獲得一個指向已釋放物件的 Gc

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `cross_thread.rs` 的 `resolve_impl()` (line 226) 和 `try_resolve_impl()` (line 313) 的 post-increment check 中添加 `is_under_construction()`：

```rust
// resolve_impl - 修改後
if gc_box.dropping_state() != 0 
    || gc_box.has_dead_flag() 
    || gc_box.is_under_construction()  // <-- 添加!
{
    GcBox::dec_ref(self.ptr.as_ptr());
    panic!("GcHandle::resolve: object was dropped after inc_ref (TOCTOU race)");
}

// try_resolve_impl - 修改後
if gc_box.dropping_state() != 0 
    || gc_box.has_dead_flag() 
    || gc_box.is_under_construction()  // <-- 添加!
{
    GcBox::dec_ref(self.ptr.as_ptr());
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 這是 bug200 修復的延續 - 當時只添加了 dropping_state 和 dead_flag 檢查
- 物件在建構中時不太可能有 cross-thread handle 指向它，但為保持一致性應該檢查

**Rustacean (Soundness 觀點):**
- 這是 defensive programming 的問題
- 雖然實際發生的機率很低，但與 Weak::try_upgrade 的實現不一致

**Geohot (Exploit 觀點):**
- 由於物件在建構中時不太可能有 handle，此問題的实际攻击面很小
- 但如果未來 GC 的内部 invariants 改变，这可能导致问题
