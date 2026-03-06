# [Bug]: Weak::try_upgrade Post-CAS 缺少 is_under_construction 檢查

**Status:** Open
**Tags:** Verified

---

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要精確時序控制：在 pre-CAS 檢查通過後、post-CAS 檢查前，物件必須從 under construction 變為非 under construction |
| **Severity (嚴重程度)** | Medium | 導致與 `upgrade()` API 不一致的行為，可能造成開發者困惑 |
| **Reproducibility (重現難度)** | Medium | 需要並發控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::try_upgrade()` (ptr.rs:1887-1953)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Weak::try_upgrade()` 應該與 `Weak::upgrade()` 行為一致：
- 當物件處於 under construction 狀態時，`upgrade()` 會 panic
- `try_upgrade()` 應該返回 `None`

### 實際行為 (Actual Behavior)
`Weak::try_upgrade()` 在以下位置存在不一致：
1. **Pre-CAS 檢查 (line 1910-1912)**：正確檢查 `is_under_construction()`，若為 true 返回 None
2. **Post-CAS 檢查 (line 1939)**：只檢查 `dropping_state()` 和 `has_dead_flag()`，**未檢查 `is_under_construction()`**

這導致以下不一致的行為：
- 如果物件在 pre-CAS 檢查時是 under construction，會正確返回 None
- 但如果在 pre-CAS 檢查後、post-CAS 檢查前，物件從 under construction 變為非 under construction，則 `try_upgrade()` 會返回 Some(Gc)，而 `upgrade()` 會 panic

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1887-1953`：

```rust
pub fn try_upgrade(&self) -> Option<Gc<T>> {
    // ...
    unsafe {
        let gc_box = &*ptr.as_ptr();

        // === Pre-CAS 檢查 (正確) ===
        if gc_box.is_under_construction() {  // line 1910
            return None;
        }

        loop {
            if gc_box.has_dead_flag() {
                return None;
            }

            if gc_box.dropping_state() != 0 {
                return None;
            }

            let current_count = gc_box.ref_count.load(Ordering::Acquire);
            // ...
            
            // CAS 成功遞增 ref_count
            if gc_box.ref_count.compare_exchange_weak(...).is_ok() {
                // === Post-CAS 檢查 (有 Bug：缺少 is_under_construction) ===
                if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {  // line 1939
                    // ...
                    return None;
                }
                // 缺少：if gc_box.is_under_construction() { ... }
                return Some(Gc { ... });
            }
        }
    }
}
```

**問題根源：**
- `Weak::upgrade()` (line 1814-1819) 有明確的 `assert!` 檢查 `is_under_construction()`
- `Weak::try_upgrade()` 的 pre-CAS 檢查有 `is_under_construction()` 檢查
- 但 post-CAS 檢查**缺少** `is_under_construction()` 檢查

**受影響的函數：**
1. `Weak::try_upgrade()` (ptr.rs:1939) - post-CAS 缺少檢查
2. `GcBoxWeakRef::try_upgrade()` (ptr.rs:679, 699) - 也有同樣問題

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論上的重現步驟：
1. 在一個執行緒中建立 Gc::new_cyclic_weak，物件處於 under construction 狀態
2. 在另一個執行緒中對同一個 GcBox 調用 `try_upgrade()`
3. 精確控制時序：讓 try_upgrade() 的 pre-CAS 檢查通過，但在 CAS 執行後、post-CAS 檢查前，完成 construction
4. 預期：返回 None
5. 實際：返回 Some(Gc)，與 upgrade() 行為不一致

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::try_upgrade()` 的 post-CAS 檢查中添加 `is_under_construction()` 檢查：

```rust
// ptr.rs:1939
if gc_box.dropping_state() != 0 
    || gc_box.has_dead_flag() 
    || gc_box.is_under_construction()  // 新增
{
    GcBox::dec_ref(ptr.as_ptr());
    return None;
}
```

同樣修復 `GcBoxWeakRef::try_upgrade()` (ptr.rs:679, 699)。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在並發場景下，物件可能從 under construction 轉換到 non-under construction。這種 race window 雖然很短，但會導致 API 行為不一致。`try_upgrade` 的設計應該與 `upgrade` 一致，只是返回 None 而不是 panic。

**Rustacean (Soundness 觀點):**
這不是嚴格的 soundness 問題（因為物件確實已經完成 construction），但會造成開發者困惑。`upgrade()` 和 `try_upgrade()` 應該有相同的語義，只是錯誤處理方式不同。

**Geohot (Exploit 攻擊觀點):**
這個 race window 很難利用，因為需要精確控制時序。但如果成功利用，可能會取得對尚未完全初始化的物件的引用，導致讀取到未初始化的記憶體。
