# [Bug]: Gc::downgrade / GcBox::as_weak / Gc::cross_thread_handle Missing Post-Increment Dead/Dropping Check (TOCTOU)

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發場景，另一執行緒在檢查和 increment 之間drop |
| **Severity (嚴重程度)** | Medium | 可能導致 weak_count / ref_count 錯誤洩漏 |
| **Reproducibility (復現難度)** | High | 需要精確的執行時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ptr.rs` - `Gc::downgrade()`, `GcBox::as_weak()`, `Gc::cross_thread_handle()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在調用 `inc_weak()` 或 `inc_ref()` 增加引用計數後，應該再次檢查對象是否已變為 dead、dropping 或 under construction 狀態，以防止 TOCTOU (Time-Of-Check-Time-Of-Use) 漏洞。

### 實際行為 (Actual Behavior)
`Gc::downgrade()`, `GcBox::as_weak()`, 和 `Gc::cross_thread_handle()` 在調用 `inc_weak()` 或 `inc_ref()` 之前檢查了 dead_flag、dropping_state 和 is_under_construction，但在 increment 之後**沒有**再次驗證這些狀態。

相比之下，`Weak::upgrade()` 正確地包含了 post-CAS 檢查：
```rust
// ptr.rs:2237-2242 - Weak::upgrade 正確做法
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::undo_inc_ref(ptr.as_ptr());
    return None;
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題程式碼

**`ptr.rs:1709-1726` - `Gc::downgrade` 函數：**
```rust
unsafe {
    assert!(
        !(*gc_box_ptr).has_dead_flag()
            && (*gc_box_ptr).dropping_state() == 0
            && !(*gc_box_ptr).is_under_construction(),
        "Gc::downgrade: cannot downgrade a dead, dropping, or under construction Gc"
    );
    (*gc_box_ptr).inc_weak();  // <-- BUG: 沒有 post-check

    if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        assert!(
            (*header.as_ptr()).is_allocated(idx),
            "Gc::downgrade: slot was swept during downgrade"
        );
    }
}
```

**`ptr.rs:1772-1791` - `GcBox::as_weak` 函數：**
```rust
let gc_box = &*ptr.as_ptr();
if gc_box.is_under_construction()
    || gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
{
    return GcBoxWeakRef { ptr: AtomicNullable::null() };
}
(*ptr.as_ptr()).inc_weak();  // <-- BUG: 沒有 post-check
// ... 只檢查了 is_allocated，沒有檢查 dead/dropping/under_construction
```

**`ptr.rs:1835-1850` - `Gc::cross_thread_handle` 函數：**
```rust
unsafe {
    assert!(
        !(*ptr.as_ptr()).has_dead_flag()
            && (*ptr.as_ptr()).dropping_state() == 0
            && !(*ptr.as_ptr()).is_under_construction(),
        "Gc::cross_thread_handle: cannot create handle for dead, dropping, or under construction Gc"
    );
    (*ptr.as_ptr()).inc_ref();  // <-- BUG: 沒有 post-check
    
    // 只檢查了 is_allocated，沒有檢查 dead/dropping/under_construction
}
```

### 邏輯缺陷
1. 執行緒 A 持有 `Gc<T>`，調用 `downgrade()` / `as_weak()` / `cross_thread_handle()`
2. 通過了 dead_flag、dropping_state、is_under_construction 檢查
3. 在調用 `inc_weak()` / `inc_ref()` 之前，執行緒 B 開始 drop 對象 (set dropping_state = 1)
4. 執行緒 A 完成 increment，但對象已經處於 dropping 狀態
5. 結果：weak_count / ref_count 被錯誤地增加，可能導致內存洩漏

### 與類似函數的比較

`Weak::upgrade()` (ptr.rs:2204-2242) 正確地實現了 post-check：
```rust
loop {
    // Pre-checks
    if gc_box.has_dead_flag() { return None; }
    if gc_box.dropping_state() != 0 { return None; }
    // ...
    
    // CAS increment
    gc_box.ref_count.compare_exchange_weak(...);
    
    // Post-CAS check - 這個關鍵的檢查在 downgrade/as_weak/cross_thread_handle 中缺失
    if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
        GcBox::undo_inc_ref(ptr.as_ptr());
        return None;
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// PoC 概念：需要並發
// 1. 執行緒 A 創建 Gc<T>，調用 Gc::downgrade()
// 2. 執行緒 A 通過了 pre-check
// 3. 執行緒 B 在 inc_weak() 之前開始 drop 對象
// 4. 執行緒 A 調用 inc_weak() - 對象正在 dropping
// 5. 結果：weak_count 錯誤增加，可能導致 weak reference 無法正確清理
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在每個受影響的函數中，於 `inc_weak()` 或 `inc_ref()` 調用**之後**添加狀態檢查：

```rust
// Gc::downgrade() - 在 inc_weak() 後添加
(*gc_box_ptr).inc_weak();

if (*gc_box_ptr).dropping_state() != 0 
    || (*gc_box_ptr).has_dead_flag() 
    || (*gc_box_ptr).is_under_construction() 
{
    // Undo the increment - 需要添加 undo_inc_weak 或直接 fetch_sub
    (*gc_box_ptr).weak_count.fetch_sub(1, Ordering::Release);
    panic!("Gc::downgrade: object became dead/dropping/under_construction after inc_weak");
}

// GcBox::as_weak() - 同樣處理
(*ptr.as_ptr()).inc_weak();

if (*ptr.as_ptr()).dropping_state() != 0 
    || (*ptr.as_ptr()).has_dead_flag() 
    || (*ptr.as_ptr()).is_under_construction() 
{
    (*ptr.as_ptr()).weak_count.fetch_sub(1, Ordering::Release);
    return GcBoxWeakRef { ptr: AtomicNullable::null() };
}

// Gc::cross_thread_handle() - 在 inc_ref() 後添加
(*ptr.as_ptr()).inc_ref();

if (*ptr.as_ptr()).dropping_state() != 0 
    || (*ptr.as_ptr()).has_dead_flag() 
    || (*ptr.as_ptr()).is_under_construction() 
{
    GcBox::dec_ref(ptr.as_ptr());
    panic!("Gc::cross_thread_handle: object became dead/dropping/under_construction after inc_ref");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 TOCTOU 漏洞可能導致 weak_count 或 ref_count 的錯誤累加。在 GC 的生命週期管理中，精確的引用計數至關重要。雖然影響不如 UAF 嚴重，但長期運行可能導致內存洩漏 - 這些錯誤的計數永遠不會被清理。

**Rustacean (Soundness 觀點):**
這不是傳統意義的 UB，但屬於並發邏輯錯誤。`Weak::upgrade()` 已經展示了正確的模式（post-CAS check），其他類似函數應該遵循同一模式。建議使用一致的檢查模式來避免此類漏洞。

**Geohot (Exploit 攻擊觀點):**
雖然是低概率的並發問題，攻擊者可以：
- 嘗試精確時序控制來觸發 race condition
- 導致內存洩漏（通過錯誤的 weak_count）
- 在極端情況下可能導致 double-free 或 use-after-free（如果配合其他漏洞）
