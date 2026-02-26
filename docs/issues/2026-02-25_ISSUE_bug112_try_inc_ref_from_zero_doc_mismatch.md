# [Bug]: try_inc_ref_from_zero 文檔與實作不一致 - 聲稱檢查 "fully alive" 但只檢查 dead

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Low` | 目前所有調用者都會先檢查 dropping_state 和 is_under_construction |
| **Severity (嚴重程度)** | `Medium` | 潛在的記憶體安全問題 - 未來可能的錯誤調用 |
| **Reproducibility (復現難度)** | `Low` | 僅當直接調用此函數且未進行安全檢查時觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::try_inc_ref_from_zero`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
根據文檔，`try_inc_ref_from_zero` 應該只允許 "fully alive (not under construction, not dead)" 的物件進行從 ref_count=0 到 ref_count=1 的原子轉換。

### 實際行為
函數只檢查：
1. `DEAD_FLAG` - 物件是否已死亡
2. `ref_count != 0` - 是否有現有的強引用

函數**沒有**檢查：
1. `dropping_state() != 0` - 物件是否正在被 drop
2. `is_under_construction()` - 物件是否正在構造中

### 文檔與實作不一致
文檔聲稱檢查 "not under construction, not dead"，但實作只檢查 "not dead"。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/ptr.rs:205-244`:

文檔說明 (lines 209-210):
```rust
/// The transition is only allowed
/// if the object is fully alive (not under construction, not dead).
```

但實作只檢查 (lines 223-230):
```rust
// Never resurrect a dead object; DEAD_FLAG means value was dropped.
if (flags & Self::DEAD_FLAG) != 0 {
    return false;
}

if ref_count != 0 {
    return false;
}
```

缺少的檢查：
- `dropping_state()` - 物件正在被 drop
- `is_under_construction()` - 物件正在構造中

雖然目前所有調用者都會先檢查這些條件（見 ptr.rs:437-441, 553-555），但：
1. 文檔具有誤導性
2. 未來可能有新的調用者忘記檢查
3. 這與函數的聲稱合約不符

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此問題是潛在的合約違反，难以直接复现：

1. 閱讀 `try_inc_ref_from_zero` 的文檔
2. 期望：函數會檢查 "not under construction, not dead"
3. 實際：函數只檢查 "not dead"
4. 如果未來有代碼直接調用此函數而未進行安全檢查，可能導致：
   - 正在構造中的物件被錯誤地使用
   - 正在被 drop 的物件被錯誤地復活

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**選項 1: 更新文檔**
修改文檔以準確反映實作：
```rust
/// Try to increment `ref_count` atomically when it is currently zero.
/// Returns true if successful, false if `ref_count` was non-zero or object is dead.
///
/// This is used by weak upgrades to atomically transition from ref=0 to ref=1
/// without racing with concurrent collection. The transition is only allowed
/// if the object is not dead.
///
/// Note: Caller must check `dropping_state()` and `is_under_construction()` 
/// before calling this function.
///
/// # Safety
///
/// Caller must ensure that if this returns true, they will properly use the
/// resulting strong reference to prevent use-after-free.
```

**選項 2: 更新實作**
在函數內部添加這些檢查（可能影響性能）：
```rust
pub(crate) fn try_inc_ref_from_zero(&self) -> bool {
    // Check for under construction
    if self.is_under_construction() {
        return false;
    }
    
    loop {
        // ... existing code ...
        
        // Check for dropping state inside the loop
        if self.dropping_state() != 0 {
            return false;
        }
        
        // ... rest of function ...
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個問題是合約/規範層面的問題。從 GC 角度來看，確保物件在允許引用計數轉換之前處於有效狀態非常重要。目前所有調用者都正確地進行了檢查，但文檔應該準確反映實作，以防止未來的錯誤使用。

**Rustacean (Soundness 觀點):**
這是一個潛在的 soundness 問題 - 如果有人根據文檔行事而未進行必要的檢查，可能會導致 use-after-free。文檔和實作應該保持一致。

**Geohot (Exploit 觀點):**
雖然目前不是直接可利用的問題（因為調用者都做了正確的檢查），但這種類型的規範不一致可能導致未來的開發者犯錯，創建可利用的漏洞。

---

## Resolution Note (2026-02-26)

**Fixed.** Updated documentation to match implementation (Option 1). The doc now states that the transition is only allowed if the object is not dead (`DEAD_FLAG` not set), and adds an explicit **Caller responsibility** note that callers must check `dropping_state()` and `is_under_construction()` before calling. No code changes to the function body.
