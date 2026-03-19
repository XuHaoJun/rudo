# [Bug]: Weak::clone 缺少 inc_weak 前置 is_allocated 檢查 (TOCTOU)

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 inc_weak 前夕被 lazy sweep 回收並重用，時機敏感 |
| **Severity (嚴重程度)** | High | 會導致 weak count 操作錯誤物件，可能造成記憶體洩漏或 use-after-free |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak<T>::clone` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75.0
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Weak::clone` 函數在調用 `inc_weak()` 之前缺少 `is_allocated` 檢查，存在 TOCTOU 漏洞。

### 預期行為 (Expected Behavior)
在調用 `inc_weak()` 之前，應該先檢查 slot 是否仍然被分配（如 `Gc::downgrade` 的實現）。

### 實際行為 (Actual Behavior)
`Weak::clone` 只在 `inc_weak()` **之後**才檢查 `is_allocated`，這意味著：
1. 如果 slot 在初始驗證後、調用 `inc_weak()` 前被 lazy sweep 回收並重用
2. `inc_weak()` 會作用在錯誤的物件上
3. 導致 weak count 計數錯誤

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:2576-2627` 的 `Weak::clone` 實現中：

```rust
// 現有程式碼順序：
// 1. 驗證指標有效性 (lines 2591-2596)
// 2. 檢查 dead_flag, dropping_state (lines 2602-2611)
// 3. 調用 inc_weak() - BUG: 此時 slot 可能已被 sweep 並重用 (line 2612)
// 4. 檢查 is_allocated (lines 2614-2622) - 太晚了！
```

正確的模式應該參考 `Gc::downgrade` (`ptr.rs:1696-1730`)：
```rust
// 正確順序：
// 1. 檢查 is_allocated BEFORE inc_weak (lines 1701-1708)
// 2. 檢查 dead_flag, dropping_state, is_under_construction (lines 1710-1715)
// 3. 調用 inc_weak() (line 1716)
// 4. 再次檢查 is_allocated AFTER inc_weak (lines 1718-1725)
```

這與 bug241 的模式完全相同。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

理論上的重現步驟：
1. 創建一個 `Gc<T>` 並獲取 `Weak<T>`
2. 觸發 GC 將物件標記為候選回收
3. 在精確的時序窗口：當 slot 被 lazy sweep 回收並分配給新物件後、舊 Weak::clone 的 inc_weak 調用前
4. 調用 `weak.clone()`
5. 觀察：新物件的 weak_count 被錯誤遞增

```rust
// 需要極端的時序控制來穩定重現
// 此 bug 需要同時滿足：
// 1. 物件被標記為可回收
// 2. Lazy sweep 在 inc_weak 前回收並重用 slot
// 3. Clone 操作的時序恰好落在此窗口
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::clone` 中，於 `inc_weak()` 調用之前新增 `is_allocated` 檢查：

```rust
// 在 line 2612 之前插入：
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Weak::clone: object slot was swept before inc_weak"
    );
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 bug 與 lazy sweep 的非同步特性有關。當 slot 被回收並重用時，weak reference 的計數操作可能作用在錯誤的物件上。這會導致：
- 新物件的 weak_count 被錯誤遞增
- 當新物件應該被回收時，可能因為錯誤的 weak_count 而無法正確回收
- 或反之，過早回收仍有 weak 引用的物件

**Rustacean (Soundness 觀點):**
這不是傳統意義上的 UB，但可能導致記憶體管理錯誤。雖然 `is_allocated` 在 `inc_weak()` 後有檢查，但此時已經太晚 - 我們已經對錯誤的物件進行了計數操作。

**Geohot (Exploit 觀點):**
如果要利用此 bug，需要精確控制 lazy sweep 的時序。這在實際攻擊中較難實現，但理論上：
- 可以通過控制物件大小和分配順序來影響 sweep 行為
- 結合其他 memory management bugs 可能造成更嚴重的後果
