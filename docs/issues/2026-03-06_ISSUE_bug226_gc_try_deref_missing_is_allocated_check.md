# [Bug]: Gc::try_deref 缺少 is_allocated 檢查 - 與 Deref::deref 行為不一致

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 Gc 存取並發執行，slot 被回收並重新分配 |
| **Severity (嚴重程度)** | High | 可能導致 Use-After-Free (UAF) - try_deref 返回 Some 但指標已無效 |
| **Reproducibility (重現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::try_deref()` in `ptr.rs:1238-1253`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`Gc::try_deref()` 方法缺少 `is_allocated` 檢查，與 `Deref::deref` 的實現不一致。

### 預期行為 (Expected Behavior)

`try_deref` 應該與 `Deref::deref` 具有相同的有效性檢查，兩者都應該在解引用前檢查 slot 是否仍然被分配。

### 實際行為 (Actual Behavior)

**`Deref::deref` 正確地有 is_allocated 檢查** (`ptr.rs:1590-1611`):
```rust
fn deref(&self) -> &Self::Target {
    // ...
    unsafe {
        if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr.cast()) {
            let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
            assert!(
                (*header.as_ptr()).is_allocated(idx),  // ✓ 有檢查!
                "Gc::deref: slot has been swept and reused"
            );
        }
        // ...
    }
}
```

**`try_deref` 缺少 is_allocated 檢查** (`ptr.rs:1238-1253`):
```rust
pub fn try_deref(gc: &Self) -> Option<&T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        if (*gc_box_ptr).has_dead_flag()
            || (*gc_box_ptr).dropping_state() != 0
            || (*gc_box_ptr).is_under_construction()
        {
            return None;
        }
        // 缺少 is_allocated 檢查!
        Some(&(*gc_box_ptr).value)
    }
}
```

對比 `Deref::deref` 實現，`try_deref` 缺少：
```rust
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr.cast()) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return None;  // slot 已被 sweep 回收
    }
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 mutator 並發執行時：
1. 物件 A 在某個 slot 被 lazy sweep 回收
2. 物件 B 在同一個 slot 被重新分配
3. Mutator 持有指向物件 A 的 `Gc` 指標
4. 呼叫 `try_deref()`:
   - 通過 `has_dead_flag()`, `dropping_state()`, `is_under_construction()` 檢查
   - **但跳過 `is_allocated` 檢查**
   - 返回 `Some` 指向已釋放的記憶體

後果：呼叫者獲得 `Some(&T)`，但指標已無效，導致 UAF。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 建立 `Gc` 物件
2. 觸發 GC，讓物件被標記為可回收
3. Lazy sweep 回收該物件的 slot
4. 在同一 slot 建立新物件
5. 呼叫 `try_deref()` - 預期返回 `None`，但會返回 `Some` 指向舊物件

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `try_deref` 中添加 `is_allocated` 檢查，與 `Deref::deref` 保持一致：

```rust
pub fn try_deref(gc: &Self) -> Option<&T> {
    let ptr = gc.ptr.load(Ordering::Acquire);
    if ptr.is_null() {
        return None;
    }
    let gc_box_ptr = ptr.as_ptr();
    
    // 新增：檢查 slot 是否仍然被分配
    if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr.cast()) {
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            return None;  // slot 已被 sweep 回收
        }
    }
    
    unsafe {
        if (*gc_box_ptr).has_dead_flag()
            || (*gc_box_ptr).dropping_state() != 0
            || (*gc_box_ptr).is_under_construction()
        {
            return None;
        }
        Some(&(*gc_box_ptr).value)
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU 漏洞，與 lazy sweep 的並發執行有關。`try_deref` 應該與 `Deref::deref` 具有相同的行為，兩者都需要檢查 slot 是否仍然被分配。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。`try_deref` 文檔說 "Returns `None` if this Gc is dead"，但當 slot 被 sweep 回收後，它會錯誤地返回 `Some`，違反了 API contract。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以嘗試構造以下場景：
1. 通過精確的執行緒調度，在 try_deref 檢查和記憶體訪問之間觸發 lazy sweep
2. 利用 slot 回收來讀取已釋放的記憶體
3. 通過控制新物件的內容來實現資訊洩露

---

## 🔗 相關 Issue

- bug197: Gc 核心方法 (as_ptr, internal_ptr, etc.) 缺少 is_allocated 檢查 - 本 bug 是 bug197 的補充，專門針對 `try_deref`
- bug207: Gc::deref 缺少 is_allocated 檢查 - 該 bug 修復了 `Deref::deref`，但未覆蓋 `try_deref`

---

## Resolution (2026-03-14)

**Outcome:** Fixed.

Added `is_allocated` check to `Gc::try_deref()` in `ptr.rs`, matching the pattern used in `try_clone`, `as_ptr`, and `Deref::deref`. The check runs before dereferencing the `GcBox` to avoid UAF when lazy sweep has reclaimed the slot. All tests pass.
