# [Bug]: GcHandle::downgrade calls inc_weak BEFORE is_under_construction check

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在物件 construction 期間呼叫 downgrade |
| **Severity (嚴重程度)** | Medium | inc_weak 在無效物件上被呼叫，導致 weak count 錯誤 |
| **Reproducibility (重現難度)** | High | 程式碼邏輯明確，可通過程式碼審查確認 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade()` (`handles/cross_thread.rs:557-596`)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcHandle::downgrade()` 應該在呼叫 `inc_weak()` 之前檢查 `is_under_construction()` 旗標，與 `resolve_impl()` 等其他函數的行為一致。

### 實際行為 (Actual Behavior)
`GcHandle::downgrade()` 在 line 561 呼叫 `inc_weak()`，但 `is_under_construction()` 檢查直到 line 590 才執行。如果物件正在 construction 中，雖然會 panic 並呼叫 `dec_weak()` 來撤銷，但 `inc_weak()` 已經在無效狀態下被呼叫。

### 對比 resolve_impl() (正確的行為)

`resolve_impl()` 在執行 `inc_ref()` 之前檢查所有旗標：

```rust
// cross_thread.rs:262-274 (resolve_impl - 正確)
let gc_box = &*self.ptr.as_ptr();
assert!(
    !gc_box.is_under_construction(),  // 檢查在 inc_ref 之前
    "GcHandle::resolve: object is under construction"
);
// ... 其他檢查 ...
gc_box.inc_ref();  // inc_ref 在所有檢查之後
```

但 `downgrade()` 的順序是錯誤的：

```rust
// cross_thread.rs:557-596 (downgrade - 錯誤)
(*self.ptr.as_ptr()).inc_weak();  // LINE 561 - inc_weak 第一個執行!

// ... 中間的檢查 ...

let gc_box = &*self.ptr.as_ptr();
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()  // LINE 590 - is_under_construction 最後才檢查!
{
    (*self.ptr.as_ptr()).dec_weak();
    panic!("GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle");
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `handles/cross_thread.rs:557-596`

**問題流程：**
1. Line 561: `inc_weak()` 被呼叫（可能在無效物件上）
2. Lines 563-572: 檢查 generation 變化（如果改變，撤銷）
3. Lines 574-585: 檢查 `is_allocated`（如果失敗，撤銷）
4. Lines 587-596: 檢查 flags（如果失敗，撤銷並 panic）

**為什麼這是 Bug：**
- Bug92 修復了「缺少 `is_under_construction()` 檢查」的問題
- 但修復不正確：檢查被放在 `inc_weak()` 之後
- 正確做法應該是先檢查，確認安全後再執行 `inc_weak()`

**影響：**
- 如果 `is_under_construction()` 為 true，`inc_weak()` 已經被呼叫
- 雖然 `dec_weak()` 會撤銷，但這個模式是錯誤的
- 與 `resolve_impl()`、`try_resolve_impl()`、`clone()` 等其他函數不一致

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 這個 bug 可以通過程式碼審查確認，不需要运行时触发
// 只要對比 downgrade() 和 resolve_impl() 的執行順序即可確認
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `is_under_construction()` 檢查移到 `inc_weak()` 之前：

```rust
// FIX: 先檢查所有狀態，再執行 inc_weak
let gc_box = &*self.ptr.as_ptr();
if gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
    || gc_box.is_under_construction()
{
    panic!("GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle");
}

// 驗證 slot 有效性（在 inc_weak 之前）
if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return WeakCrossThreadHandle {
            weak: GcBoxWeakRef::null(),
            origin_tcb: Weak::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        };
    }
}

// 最後才執行 inc_weak（在所有檢查通過之後）
let pre_generation = (*self.ptr.as_ptr()).generation();
(*self.ptr.as_ptr()).inc_weak();

// 檢查 generation 是否改變（用於檢測 slot 重用）
if pre_generation != (*self.ptr.as_ptr()).generation() {
    (*self.ptr.as_ptr()).dec_weak();
    // ... return null handle ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
downgrade() 應該在其他操作之前驗證物件狀態。inc_weak() 修改 internal 狀態，應該在確保物件有效後才執行。

**Rustacean (Soundness 觀點):**
這是 API 一致性問題。雖然最終會 panic 並撤銷操作，但執行順序錯誤表示代碼結構有缺陷。

**Geohot (Exploit 觀點):**
如果物件在 construction 期間被 downgrade，weak count 可能已經被修改。即使有撤銷機制，這種模式可能在並髮環境下造成問題。

---

## 相關 Issue

- Bug92: GcHandle::downgrade 缺少 is_under_construction 檢查 (已修復，但不完整)
- Bug351: GcHandle downgrade generation check (相關)
- Bug345: resolve is_allocated check before inc_ref (相關)