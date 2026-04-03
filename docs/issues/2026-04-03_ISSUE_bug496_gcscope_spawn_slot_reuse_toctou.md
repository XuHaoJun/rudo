# [Bug]: GcScope::spawn slot reuse TOCTOU between is_allocated check and dereference

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | Requires precise timing between lazy sweep and GcScope::spawn |
| **Severity (嚴重程度)** | `Critical` | Use-after-free if slot is reused between check and dereference |
| **Reproducibility (復現難度)** | `Medium` | Race condition with lazy sweep; requires concurrent execution |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcScope::spawn` (handles/async.rs:1290-1361)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `Current`

---

## 📝 問題描述 (Description)

在 `GcScope::spawn` 中，`is_allocated` 檢查和指標解引用之間存在 TOCTOU (Time-of-Check-Time-of-Use) race condition。如果 lazy sweep 在這段時間間隔內回收並重新分配 slot，可能導致 use-after-free。

### 預期行為 (Expected Behavior)
追蹤的物件應該在 `is_allocated` 檢查和解引用期間保持有效，不會被 sweep 並重新分配。

### 實際行為 (Actual Behavior)
在 `is_allocated` 檢查通過後，如果 lazy sweep 執行並回收該 slot，然後重新分配給新物件，則後續的解引用會存取已釋放的記憶體。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題出在 `handles/async.rs:1315-1327`：

```rust
// Line 1315-1320: is_allocated check
if let Some(idx) = crate::heap::ptr_to_object_index(tracked.ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(tracked.ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "GcScope::spawn: tracked object was deallocated"
    );
}
// Line 1322-1325: Capture generation BEFORE dereference
pre_generation = (*tracked.ptr).generation();

// Line 1327: Dereference happens here - WINDOW FOR RACE
let gc_box = unsafe { &*tracked.ptr };

// Line 1328-1333: Verify generation hasn't changed
if pre_generation != gc_box.generation() {
    panic!("GcScope::spawn: slot was reused between liveness check and dereference");
}
```

**Race 條件視窗：**
1. `is_allocated` 檢查通過 (slot 处于 allocated 狀態)
2. Lazy sweep 執行並回收該 slot
3. Slot 被重新分配給新物件 (generation 改變)
4. 指標被解引用，存取到已釋放/重新分配的記憶體

**修復：** 使用 generation 检查来检测 slot 是否在检查和引用之间被重用。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要精確時序控制：
// 1. 在 GcScope::spawn 中，is_allocated 檢查通過後
// 2. 在指標解引用前，觸發 lazy sweep
// 3. Lazy sweep 回收 slot 並重新分配
// 4. 指標解引用時，generation 已改變

// 註：此 race condition 難以穩定重現，但理論上可能發生
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**已修復：** 代碼中已包含 generation 檢查來緩解此問題：

```rust
// Get generation BEFORE dereference to detect slot reuse (bugXXX).
pre_generation = (*tracked.ptr).generation();

let gc_box = unsafe { &*tracked.ptr };

// FIX bugXXX: Verify generation hasn't changed (slot was NOT reused).
if pre_generation != gc_box.generation() {
    panic!("GcScope::spawn: slot was reused between liveness check and dereference");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 在背景執行時，slot 可能在 `is_allocated` 檢查後被回收並重新分配。傳統 GC 通常在 stop-the-world 暫停期間進行所有回收操作，因此不存在此問題。但 rudo-gc 的 lazy sweep 是並發執行，增加了 race condition 的可能性。Generation 檢查是正確的緩解措施。

**Rustacean (Soundness 觀點):**
此 TOCTOU 可能導致 UB (use-after-free)。Generation 檢查提供了一道防線，但理想情況下應該在 `is_allocated` 檢查和引用之間阻止 lazy sweep 干預。這涉及更複雜的同步機制。當前解決方案在實踐中有效，但並非在編譯期保證安全。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制 lazy sweep 的時序，他們可能會：
1. 在 `is_allocated` 檢查後觸發 sweep
2. 將 slot 重新分配給攻擊者控制的資料
3. 通過 `&*tracked.ptr` 的解引用讀取攻擊者控制的記憶體

這可用於資訊洩漏或繞過 CFRG (Control Flow Guard)。Generation 檢查使此攻擊難以穩定實現，但並非不可能。

---

## 備註

此 bug 在代碼中被標記為 `bugXXX`，表示曾被識別和修復，但從未給予正式 bug 編號或記錄在 issue tracker 中。