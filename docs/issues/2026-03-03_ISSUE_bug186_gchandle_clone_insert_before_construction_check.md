# [Bug]: GcHandle::clone() 在 is_under_construction 檢查前插入 root 導致記憶體洩漏

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 物件構造過程中進行 clone 操作相對少見 |
| **Severity (嚴重程度)** | Medium | 導致 root entry 記憶體洩漏，且檢查在錯誤時機進行 |
| **Reproducibility (復現難度)** | Medium | 需要在物件構造過程中精確時機呼叫 clone |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** GcHandle::clone (handles/cross_thread.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`GcHandle::clone()` 在檢查 `is_under_construction()` **之前**先將新的 handle entry 插入到 `roots.strong` HashMap 中。

### 預期行為

應該先檢查物件是否處於構造中（`is_under_construction()`），確認安全後再插入 root entry。與 `Gc::cross_thread_handle()` 的順序一致（該函數正確地在 insert 前進行檢查）。

### 實際行為

1. Line 344: 獲取 lock
2. Line 345: 檢查原有 handle 是否存在
3. Line 348: 分配 new_id
4. **Line 349: 插入 new entry 到 roots.strong** ← 這時就已經添加進去了！
5. Lines 352-359: 獲取 gc_box 並執行 assert 檢查（包括 is_under_construction）

如果 is_under_construction 為 true，assert 會 panic，但此時 new entry 已經在 HashMap 中了，導致記憶體洩漏（orphaned root entry）。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/handles/cross_thread.rs` 的 `GcHandle::clone()` 函數中：

```rust
// lines 344-360
let mut roots = tcb.cross_thread_roots.lock().unwrap();
if !roots.strong.contains_key(&self.handle_id) {
    panic!("cannot clone an unregistered GcHandle");
}
let new_id = roots.allocate_id();
roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());  // ← 先插入！
// inc_ref for new handle while holding lock (prevents TOCTOU with unregister)
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    assert!(
        !gc_box.has_dead_flag()
            && gc_box.dropping_state() == 0
            && !gc_box.is_under_construction(),  // ← 後檢查！
        "GcHandle::clone: cannot clone a dead, dropping, or under construction GcHandle"
    );
    gc_box.inc_ref();
}
```

正確的順序應該參考 `Gc::cross_thread_handle()` (ptr.rs lines 1471-1486)：
1. 先進行所有安全檢查（包括 is_under_construction）
2. 然後再插入 root entry

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要構造一個在 GcBox 構造過程中的 clone 場景。理論上可以通過以下方式觸發：

1. 在 `Gc::new_cyclic_weak` 的 closure 內部嘗試 clone GcHandle
2. 這會導致 is_under_construction 為 true
3. Clone 會 panic，但 root entry 已經被添加

```rust
// PoC 概念
let gc = Gc::new_cyclic_weak(|weak_ref| {
    // 這裡 gc 處於 under_construction 狀態
    let handle = gc.cross_thread_handle();
    // 嘗試 clone 會 panic，且會 leak root entry
    let _cloned = handle.clone();
});
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `is_under_construction` 檢查移到 `roots.strong.insert()` 之前，與 `Gc::cross_thread_handle()` 保持一致：

```rust
let mut roots = tcb.cross_thread_roots.lock().unwrap();
if !roots.strong.contains_key(&self.handle_id) {
    panic!("cannot clone an unregistered GcHandle");
}

// 先進行安全檢查（包含 is_under_construction）
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    assert!(
        !gc_box.has_dead_flag()
            && gc_box.dropping_state() == 0
            && !gc_box.is_under_construction(),
        "GcHandle::clone: cannot clone a dead, dropping, or under construction GcHandle"
    );
}

// 檢查通過後再插入 root entry
let new_id = roots.allocate_id();
roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());

// 最後 inc_ref
unsafe {
    (*self.ptr.as_ptr()).inc_ref();
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- 雖然觸發條件較少見，但這是 GC root 管理的一致性問題
- Orphaned root entry 會導致 GC 無法回收相關物件，造成長期記憶體洩漏
- 與 `Gc::cross_thread_handle()` 的實現不一致，後者正確地先檢查後插入

**Rustacean (Soundness 觀點):**
- 這不是傳統意義的 soundness 問題（不會導致 UAF）
- 但造成不一致的 API 行為和資源洩漏
- 違反 "fail fast" 原則 - 應該在造成任何副作用（insert）前檢查

**Geohot (Exploit 觀點):**
- 記憶體洩漏可用於 DoS 攻擊
- 攻擊者可能透過大量觸發此場景來耗盡記憶體
- 需要在物件構造過程中精確時機，難以穩定利用

---

## Resolution (2026-03-03)

**Fixed.** Moved the `has_dead_flag`, `dropping_state`, and `is_under_construction` checks before `roots.strong.insert()` in `GcHandle::clone()`, matching the order in `Gc::cross_thread_handle()`. This prevents orphaned root entries when the assert panics. All cross_thread_handle and GcHandle tests pass.
