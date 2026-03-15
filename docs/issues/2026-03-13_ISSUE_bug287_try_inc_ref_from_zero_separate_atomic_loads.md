# [Bug]: try_inc_ref_from_zero 分离加载 ref_count 和 weak_count 导致 TOCTOU 竞争条件

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要特定的並發時序：兩個線程同時修改 ref_count 和 weak_count |
| **Severity (嚴重程度)** | High | 可能導致對已死亡物件的錯誤復活 (resurrection) 或無法正確復活 |
| **Reproducibility (復現難度)** | Medium | 需要多線程並發時序，單線程無法重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::try_inc_ref_from_zero()`, `ptr.rs:248-249`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
`try_inc_ref_from_zero()` 應該以原子方式加載 `ref_count` 和 `weak_count`，確保這兩個相關的計數器在整個檢查過程中保持一致。

### 實際行為
函數使用兩個獨立的 atomic load 來讀取 `ref_count` 和 `weak_count_raw`：

```rust
let ref_count = self.ref_count.load(Ordering::Acquire);      // Line 248
let weak_count_raw = self.weak_count.load(Ordering::Acquire); // Line 249

let flags = weak_count_raw & Self::FLAGS_MASK;  // 使用 weak_count_raw 的 flags
// ... 後續檢查使用上面加載的兩個值
```

這創造了一個 TOCTOU (Time-of-Check to Time-of-Use) 競爭條件：
1. 線程 A 加載 `ref_count` = 0
2. 線程 B 同時修改 `weak_count`（例如創建或刪除 weak reference）
3. 線程 A 加載 `weak_count_raw` - 但現在與之前加載的 `ref_count` 不一致

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:246-285` 的 `try_inc_ref_from_zero()` 函數中：

```rust
pub(crate) fn try_inc_ref_from_zero(&self) -> bool {
    loop {
        let ref_count = self.ref_count.load(Ordering::Acquire);      // LINE 248
        let weak_count_raw = self.weak_count.load(Ordering::Acquire); // LINE 249

        let flags = weak_count_raw & Self::FLAGS_MASK;  // LINE 251 - 使用從 weak_count_raw 提取的 flags

        // 這些檢查使用可能不一致的值！
        if (flags & Self::DEAD_FLAG) != 0 {  // LINE 254
            return false;
        }
        // ...
        if ref_count != 0 {  // LINE 269
            return false;
        }
        // ...
    }
}
```

**問題**：
1. `ref_count` 和 `weak_count_raw` 是兩個獨立的 atomic 變量
2. 它們被加載為兩個獨立的操作，中間沒有同步
3. 另一個線程可以在兩次加載之間修改其中任何一個變量
4. 這導致 flags 檢查和 ref_count 檢查使用不一致的狀態

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 是潛在的，需要特定時序：
1. 線程 A 調用 `try_inc_ref_from_zero()`
2. 線程 A 讀取 `ref_count` = 0
3. 線程 B 同時修改 `weak_count`（例如，從有 weak reference 變為無 weak reference）
4. 線程 A 讀取 `weak_count_raw` - 但 flags 可能與當前 ref_count 狀態不一致
5. 導致錯誤的復活行為

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

使用單一的 atomic 操作來同時讀取 ref_count 和 weak_count，或者確保在讀取後再次驗證一致性：

**選項 1**: 在加載後進行第二次驗證
```rust
pub(crate) fn try_inc_ref_from_zero(&self) -> bool {
    loop {
        let ref_count = self.ref_count.load(Ordering::Acquire);
        let weak_count_raw = self.weak_count.load(Ordering::Acquire);
        
        let flags = weak_count_raw & Self::FLAGS_MASK;
        
        // 第一次檢查
        if (flags & Self::DEAD_FLAG) != 0 || self.dropping_state() != 0 || self.is_under_construction() || ref_count != 0 {
            return false;
        }
        
        // 嘗試 CAS 失敗後，重新讀取以驗證狀態未變
        match self.ref_count.compare_exchange_weak(0, 1, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => {
                // 成功後再次驗證 weak_count 狀態
                let new_weak_count = self.weak_count.load(Ordering::Acquire);
                let new_flags = new_weak_count & Self::FLAGS_MASK;
                if (new_flags & Self::DEAD_FLAG) != 0 || self.dropping_state() != 0 {
                    // 回滾並返回 false
                    self.dec_ref();
                    return false;
                }
                return true;
            }
            Err(new_count) => {
                if new_count != 0 {
                    return false;
                }
            }
        }
    }
}
```

**選項 2**: 使用更強的 atomic 同步（如果 Rust 支持）

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent D. Dybvig (GC 架構觀點):**
GC 的正確性依賴於準確的引用計數狀態。當 ref_count 和 weak_count 不一致時，可能導致： (1) 錯誤地允許對已死亡物件的復活，導致記憶體洩漏； (2) 錯誤地拒絕對有效物件的復活，導致 use-after-free。

**Rustacean (Soundness 觀點):**
雖然不直接是傳統意義上的 UB，但讀取不一致的 atomic 狀態可能導致： (1) 對處於無效狀態的物件進行操作； (2) 在並發場景下可能導致邏輯錯誤。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者能控制 ref_count 和 weak_count 的修改時序，他們可能利用這個 TOCTOU 窗口來： (1) 強制復活一個已經完全釋放的物件； (2) 阻止對有效物件的合法復活。結合其他漏洞，這可能導致更嚴重的記憶體破壞。

---

## 驗證記錄

**驗證日期:** 2026-03-14
**驗證人員:** opencode

### 驗證結果

確認 bug 存在於 `ptr.rs:248-249`:
- Line 248: `let ref_count = self.ref_count.load(Ordering::Acquire);`
- Line 249: `let weak_count_raw = self.weak_count.load(Ordering::Acquire);`

這兩個獨立的 atomic load 創造了 TOCTOU 競爭條件，與 issue 描述的問題完全一致。

---

## Resolution (2026-03-15)

**Fix applied:** Post-CAS verification in `try_inc_ref_from_zero()` (`ptr.rs`). After CAS(0,1) succeeds, re-read `weak_count` and `dropping_state`. If `DEAD_FLAG` or `dropping_state` is set, rollback via `ref_count.fetch_sub(1, Release)` and return `false`. This closes the TOCTOU window between the two separate loads. Callers (Weak::upgrade, GcBoxWeakRef::upgrade) already had post-CAS checks; the fix adds defense-in-depth inside the function itself.
