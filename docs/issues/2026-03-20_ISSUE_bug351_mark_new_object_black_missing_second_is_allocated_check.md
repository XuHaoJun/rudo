# [Bug]: mark_new_object_black TOCTOU - missing is_allocated check before gc_box dereference

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要並發：lazy sweep 與標記同時執行 |
| **Severity (嚴重程度)** | Medium | 可能導致讀取已釋放記憶體，潛在 UAF |
| **Reproducibility (復現難度)** | High | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `mark_new_object_black`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在 `mark_new_object_black` 中，應該在 gc_box 解引用之前再次檢查 `is_allocated`，確保 slot 在 TOCTOU 競爭條件下（lazy sweep 在檢查和解引用之間回收 slot 並分配新物件）仍然有效。

### 實際行為 (Actual Behavior)
當前順序：
1. 檢查 `is_allocated(idx)` (line 1018)
2. **Race Window**: lazy sweep 可能在此時回收 slot 並分配新物件
3. 從 gc_box 讀取 `is_under_construction()` (line 1024-1025) - 解引用可能已釋放的記憶體！

這與 `mark_object_black` (bug307) 的修復模式不同，後者有兩個 `is_allocated` 檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/incremental.rs` 的 `mark_new_object_black` 函數中：

```rust
pub fn mark_new_object_black(ptr: *const u8) -> bool {
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(ptr.cast()) {
            let header = crate::heap::ptr_to_page_header(ptr);
            if !(*header.as_ptr()).is_allocated(idx) {  // 第一次檢查
                return false;
            }
            // BUG: 缺少第二次 is_allocated 檢查！
            let gc_box = &*ptr.cast::<GcBox<()>>();  // 可能解引用已釋放記憶體
            if gc_box.is_under_construction() {
                return false;
            }
            // ...
        }
    }
}
```

對比 `mark_object_black` (lines 1056-1071) 的正確模式：

```rust
// 第一次檢查
if !(*h).is_allocated(idx) {
    return None;
}

// 第二次檢查 - 在解引用之前
if !(*h).is_allocated(idx) {
    return None;
}

// 現在可以安全解引用
let gc_box = &*ptr.cast::<GcBox<()>>();
```

**競爭條件情境：**
1. Handle 指向 slot X 中的物件 A
2. `is_allocated(idx)` 返回 `true`（slot 被 A 佔用）
3. Lazy sweep 在此時回收 slot X 並分配新物件 B
4. `gc_box` 解引用讀取 B 的記憶體（UAF！）
5. B 的 `is_under_construction()` 可能為 false（新物件）
6. 錯誤地繼續標記流程

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的執行緒交錯控制：

```rust
// 概念驗證 - 需要 TSan 或極端的時序控制
// 執行緒 1: 調用 mark_new_object_black() 指向 A
// 執行緒 2: lazy sweep + 在相同 slot 分配 B
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 gc_box 解引用之前添加第二次 `is_allocated` 檢查，與 `mark_object_black` (bug307) 的模式一致：

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    return false;
}
// 新增：第二次檢查，在解引用之前
if !(*header.as_ptr()).is_allocated(idx) {
    return false;
}
let gc_box = &*ptr.cast::<GcBox<()>>();
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU race condition。Lazy sweep 在檢查和解引用之間執行會導致 UAF。`mark_object_black` 已經有這個修復（bug307），但 `mark_new_object_black` 遺漏了相同的修復。

**Rustacean (Soundness 觀點):**
這可能導致 UAF - 解引用已釋放的記憶體。如果新物件的記憶體佈局與舊物件重疊，可能會讀取到無效的狀態。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制 lazy sweep 的時序：
1. 將物件 A 標記為不可達
2. 觸發 lazy sweep 回收 slot
3. 在同一個 slot 分配受控的新物件 B
4. mark_new_object_black 會錯誤地解引用 B 的記憶體
5. 可能導致進一步的記憶體損壞

---

## 🔗 相關 Issue

- bug238: mark_object_black missing is_under_construction check
- bug272: mark_new_object_black post-mark TOCTOU
- bug307: mark_object_black missing is_allocated check before dereference
