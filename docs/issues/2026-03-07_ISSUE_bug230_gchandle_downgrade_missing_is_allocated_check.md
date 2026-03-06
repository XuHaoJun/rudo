# [Bug]: GcHandle::downgrade Missing is_allocated Check

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 handle 建立後、downgrade 前發生 lazy sweep slot 重用 |
| **Severity (嚴重程度)** | High | 可能訪問已釋放記憶體的 GcBox，導致 UAF |
| **Reproducibility (重現難度)** | Medium | 需要精確控制 GC timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade` (cross_thread.rs:290-334)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcHandle::downgrade` 函數在dereference GcBox 指標之前，沒有檢查該 slot 是否仍然 allocated。這與 Bug 229 (GcBox::as_weak) 和其他類似的 bug 模式相同。

### 預期行為 (Expected Behavior)
`GcHandle::downgrade` 應該在 dereference `self.ptr` 之前檢查該 slot 是否仍然 allocated，確保不會訪問已釋放的記憶體。

### 實際行為 (Actual Behavior)
`GcHandle::downgrade` 直接dereference指標並檢查：
- `has_dead_flag()`
- `dropping_state()`
- `is_under_construction()`

但缺少 `is_allocated` 檢查。如果 slot 被 sweep 後重用，可能會訪問新物件的 GcBox header，導致錯誤的行為。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `cross_thread.rs:302-310`：

```rust
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
```

問題：
1. 直接dereference `self.ptr.as_ptr()` 而沒有檢查 `is_allocated`
2. 檢查的是新物件的狀態（如果 slot 被重用），而不是原始物件
3. 這可能導致在已釋放的記憶體上執行操作

對比 `GcBoxWeakRef::clone` (ptr.rs:603-611) 有正確的檢查：
```rust
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*ptr.as_ptr()).dec_weak();
        return Self {
            ptr: AtomicNullable::null(),
        };
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 建立 Gc object 並取得 GcHandle
2. 觸發 GC，使用 lazy sweep 回收該 object
3. 新 object 在同一個 slot 被分配
4. 呼叫 `GcHandle::downgrade()`
5. 預期：返回 None 或正確處理
6. 實際：可能訪問錯誤的 GcBox header

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcHandle::downgrade` 中新增 `is_allocated` 檢查：

```rust
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    
    // 新增：檢查 slot 是否仍然 allocated
    if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            panic!("GcHandle::downgrade: slot has been deallocated and reused");
        }
    }
    
    assert!(
        !gc_box.has_dead_flag()
            && gc_box.dropping_state() == 0
            && !gc_box.is_under_construction(),
        "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle"
    );
    gc_box.inc_weak();
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 lazy sweep 實現中，slot 可能被回收並立即重用。如果在 downgrade 時沒有檢查 is_allocated，可能會讀取到新物件的 header 資訊，導致錯誤的 weak reference 計數或狀態檢查。

**Rustacean (Soundness 觀點):**
這可能導致 use-after-free 類型的問題。當 slot 被重用後，舊的 GcBox header 已經無效，讀取可能會得到垃圾數據。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能透過控制 GC timing 來觸發這個問題，進一步利用記憶體佈局進行攻擊。
