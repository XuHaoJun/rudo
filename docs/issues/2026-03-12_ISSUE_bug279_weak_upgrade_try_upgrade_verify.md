# [Bug]: Weak::upgrade and Weak::try_upgrade missing is_allocated check after CAS (DUPLICATE - verify status)

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 weak upgrade 並發 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free 或返回錯誤物件 |
| **Reproducibility (重現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `Weak::upgrade`, `Weak::try_upgrade` in `crates/rudo-gc/src/ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
`Weak::upgrade()` 和 `Weak::try_upgrade()` 應該在成功 CAS 增強 ref_count 後執行 `is_allocated` 檢查，以防止 lazy sweep 回收並重新分配插槽後，返回指向錯誤物件的 Gc。

### 實際行為
在 ptr.rs 中，`Weak::upgrade` (lines 1870-1929) 和 `Weak::try_upgrade` (lines 1949-2015) 在 CAS 成功後只檢查 `dropping_state()` 和 `has_dead_flag()`，但沒有檢查 `is_allocated()`。

這與 `GcBoxWeakRef::upgrade()` 的行為不一致，後者已經有此檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 Weak 指標存儲在可能比 GC 物件壽命更長的資料結構中，且 lazy sweep 並發執行時：

1. 插槽中的物件 A 被 lazy sweep 回收（釋放）
2. 物件 B 被分配到同一個插槽
3. Mutator 對物件 B 的 Weak 調用 `upgrade()` 或 `try_upgrade()`
4. 舊指標（現在指向物件 B 的插槽）通過所有旗標檢查
5. 解引用該插槽 - 但裡面是物件 B 的資料！
6. 返回指向錯誤物件的 Gc 或讀取無效記憶體

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試環境：
1. Store Weak in a data structure that outlives the GC object
2. Trigger lazy sweep to reclaim original object
3. Allocate new object in same slot
4. Call Weak::upgrade() or Weak::try_upgrade()
5. Observe incorrect behavior (wrong object or invalid memory access)

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::upgrade()` 和 `Weak::try_upgrade()` 的 CAS 成功後添加 `is_allocated` 檢查，與 `GcBoxWeakRef::upgrade()` 的模式一致：

```rust
// 在 ptr.rs 中，Weak::upgrade (line ~1901) 和 Weak::try_upgrade (line ~1990)
// CAS 成功後添加：
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        GcBox::dec_ref(ptr.as_ptr());
        return None;
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Lazy sweep 會在 GC 期間回收未標記的物件，但 Weak 指標可能仍然存在。此時若呼叫 upgrade，可能會返回指向已釋放記憶體的 Gc。

**Rustacean (Soundness 觀點):**
這是經典的 UAF 問題。在返回 Gc 前必須檢查物件是否仍為有效配置。

**Geohot (Exploit 觀點):**
若攻擊者能控制重新分配的內容，可能利用此漏洞進行記憶體佈局操縱。

---

## 📌 Note

This is a duplicate of bug236 to verify the bug still exists in the current codebase.

**Verified present in code at:**
- `Weak::upgrade`: ptr.rs:1870-1929 (no is_allocated check after CAS at line 1901-1927)
- `Weak::try_upgrade`: ptr.rs:1949-2015 (no is_allocated check after CAS at line 1990-2011)
