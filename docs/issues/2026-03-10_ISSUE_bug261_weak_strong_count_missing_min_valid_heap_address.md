# [Bug]: Weak::strong_count() 與 Weak::weak_count() 缺少 MIN_VALID_HEAP_ADDRESS 檢查

**Status:** Invalid
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要傳入低於最小堆疤位址的指標 |
| **Severity (嚴重程度)** | Medium | 可能解引用無效記憶體位址 |
| **Reproducibility (重現難度)** | Low | 需要人為構造惡意指標 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::strong_count()`, `Weak::weak_count()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Weak::strong_count()` 與 `Weak::weak_count()` 應該在解引用指標之前驗證指標位址有效性，應包含：
1. 對齊檢查 (`ptr_addr % alignment != 0`)
2. 最小堆疤位址檢查 (`ptr_addr < MIN_VALID_HEAP_ADDRESS`)
3. GC box 有效性檢查 (`is_gc_box_pointer_valid()`)

### 實際行為 (Actual Behavior)
`Weak::strong_count()` (ptr.rs:2090-2111) 與 `Weak::weak_count()` (ptr.rs:2118-2139) 只檢查對齊：
```rust
if ptr_addr % alignment != 0 {
    return 0;
}
```

缺少 `MIN_VALID_HEAP_ADDRESS` 檢查，與其他類似函數不一致。

---

## 🔬 根本原因分析 (Root Cause Analysis)

對比其他函數的正確實現：

**GcBoxWeakRef::clone() (ptr.rs:523-525):**
```rust
if ptr_addr % alignment != 0 || ptr_addr < MIN_VALID_HEAP_ADDRESS {
    return None;
}
```

**Weak::strong_count() (ptr.rs:2096-2098):**
```rust
if ptr_addr % alignment != 0 {  // 缺少 MIN_VALID_HEAP_ADDRESS 檢查！
    return 0;
}
```

缺少 `MIN_VALID_HEAP_ADDRESS` 檢查可能導致函數解引用低於最小有效堆疤位址的指標，雖然 `is_gc_box_pointer_valid()` 可能會捕捉大多數情況，但不一致的檢查會造成潜在的安全漏洞。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 建立一個 `Weak<T>` 實例
2. 人為設定其內部指標為小於 4096 的位址（如 1024）
3. 呼叫 `Weak::strong_count()` 或 `Weak::weak_count()`
4. 由於缺少 MIN_VALID_HEAP_ADDRESS 檢查，可能會嘗試解引用無效位址

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::strong_count()` 與 `Weak::weak_count()` 中加入 `MIN_VALID_HEAP_ADDRESS` 檢查：

```rust
// 在 Weak::strong_count() 中，修改:
let ptr_addr = ptr.as_ptr() as usize;
let alignment = std::mem::align_of::<GcBox<T>>();
if ptr_addr % alignment != 0 || ptr_addr < MIN_VALID_HEAP_ADDRESS {
    return 0;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
MIN_VALID_HEAP_ADDRESS (4096) 是堆疤管理的最小有效位址。跳過此檢查會使函數可能接觸到非堆疤記憶體區域，導致未定義行為。

**Rustacean (Soundness 觀點):**
缺少明確的位址邊界檢查會降低程式碼的安全性。雖然 is_gc_box_pointer_valid 可能會捕捉大多数無效指標，但明確檢查 MIN_VALID_HEAP_ADDRESS 是一個簡單有效的防禦層。

**Geohot (Exploit 觀點):**
攻擊者可能嘗試利用缺少邊界檢查的指標進行惡意操作。即使在正常使用情況下不太可能觸發，這是一個潛在的攻击面。

---

## 相關問題 (Related Issues)

- bug208: Weak::strong_count 缺少 is_gc_box_pointer_valid 檢查（不同問題）
- bug52: Weak::strong_count 缺少 dropping_state 檢查（已修復）
- bug117: Weak::strong_count 缺少 is_under_construction 檢查（已修復）

---

## 驗證記錄 (Resolution)

**驗證日期:** 2026-03-15

### 驗證結果

Issue misidentified. The current implementation in `ptr.rs` (lines 2313–2367) already includes all three checks for both `Weak::strong_count()` and `Weak::weak_count()`:

1. **Alignment check**: `ptr_addr % alignment != 0`
2. **MIN_VALID_HEAP_ADDRESS check**: `ptr_addr < MIN_VALID_HEAP_ADDRESS`
3. **is_gc_box_pointer_valid check**: `!is_gc_box_pointer_valid(ptr_addr)`

The issue describes code that no longer exists; the fix was likely applied in a prior resolution (e.g. bug208 or related). No code changes required.
