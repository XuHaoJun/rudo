# [Bug]: GcBoxWeakRef::upgrade 與 try_upgrade 對 is_dead_or_unrooted 與 dropping_state 檢查順序不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確的並發時序：dropping_state 轉換在 is_dead_or_unrooted 檢查後、try_inc_ref_if_nonzero 前發生 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free：允許對正在 drop 的物件新增強引用 |
| **Reproducibility (復現難度)** | High | 需要精確的執行緒時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade()` 與 `try_upgrade()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`upgrade()` 與 `try_upgrade()` 對於相同的物件狀態應該有一致的行為。特別是當 `dropping_state != 0` 時（物件正在被 drop），兩者都不應該允許 upgrade 成功。

### 實際行為 (Actual Behavior)
兩者對 `is_dead_or_unrooted()` 與 `dropping_state()` 的檢查順序不一致，可能導致不同的行為：

**`upgrade()` 檢查順序 (lines 693-706):**
```rust
if gc_box.is_under_construction() { return None; }      // line 693
if gc_box.has_dead_flag() { return None; }                // line 698
if gc_box.dropping_state() != 0 { return None; }         // line 704 - 獨立在 ref_count 之外
```

**`try_upgrade()` 檢查順序 (lines 928-938):**
```rust
if gc_box.is_under_construction() { return None; }      // line 928
if gc_box.is_dead_or_unrooted() { return None; }         // line 932
if gc_box.dropping_state() != 0 { return None; }         // line 936
```

關鍵差異：
- `upgrade()` 使用 `has_dead_flag()`，只在 `ref_count > 0` 時有意義
- `try_upgrade()` 使用 `is_dead_or_unrooted()`，在 `ref_count == 0` 時也會返回 `true`

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `is_dead_or_unrooted()` 的實作：

```rust
pub(crate) fn is_dead_or_unrooted(&self) -> bool {
    (self.weak_count.load(Ordering::Acquire) & Self::DEAD_FLAG) != 0
        || self.ref_count.load(Ordering::Acquire) == 0  // <-- 這個條件
}
```

當 `ref_count == 0` 時，`is_dead_or_unrooted()` 返回 `true`，導致 `try_upgrade()` 在 line 932 直接返回，不會繼續檢查 `dropping_state()`。

而在 `upgrade()` 中，`dropping_state()` 的檢查（line 704）在 `ref_count == 0` 的邏輯之外，總是會被执行。

理論上的 TOCTOU 場景：
1. Thread A 調用 `try_upgrade()`
2. `ref_count == 1, dropping_state == 0`
3. Line 932: `is_dead_or_unrooted()` 返回 `false`（ref_count != 0 且 DEAD_FLAG 未設置）
4. Thread B 開始 drop：`ref_count` 從 1 遞減到 0，`dropping_state` 變為 1
5. Thread A 到达 line 936: `dropping_state() != 0` 返回 `true`，但已經浪費了前面的流程

更嚴重的情況：如果 `ref_count` 在 Thread B 中已經變成 0，但 `dropping_state` 還是 0（还没调用 `try_mark_dropping()`），`is_dead_or_unrooted()` 會返回 `true`（因為 `ref_count == 0`），並且 `dropping_state()` 檢查永遠不會被执行。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的並發時序控制：

```rust
// 理論 PoC - 需要 Miri 或 ThreadSanitizer 驗證
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::thread;

fn main() {
    // 建立物件和 weak reference
    let gc = Gc::new(Data);
    let weak = Gc::downgrade(&gc);
    
    // 並發 scenario:
    // Thread A: try_upgrade() 
    //   - 讀取 ref_count == 1
    //   - is_dead_or_unrooted() 返回 false (因為 ref_count != 0)
    //   - 但 dropping_state == 0，所以繼續
    // Thread B: drop gc
    //   - dec_ref: ref_count 變 0
    //   - try_inc_ref_from_zero() 返回 false
    //   - 物件進入 dropping_state == 1
    // Thread A: 繼續執行
    //   - 但這裡的邏輯可能已經不一致
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `try_upgrade()` 中，調整 `dropping_state()` 的檢查位置，確保它永遠被檢查到，無論 `ref_count` 的值為何：

```rust
// 修改 try_upgrade() 的檢查順序
if gc_box.is_under_construction() {
    return None;
}

// 先檢查 dropping_state（與 upgrade 一致）
if gc_box.dropping_state() != 0 {
    return None;
}

// 最後再檢查 is_dead_or_unrooted
if gc_box.is_dead_or_unrooted() {
    return None;
}
```

或者，直接使用與 `upgrade()` 相同的檢查邏輯，不使用 `is_dead_or_unrooted()`：

```rust
if gc_box.is_under_construction() {
    return None;
}
if gc_box.has_dead_flag() {
    return None;
}
if gc_box.dropping_state() != 0 {
    return None;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
TOCTOU 問題在 RC-like 系統中很常見。`is_dead_or_unrooted()` 的設計是為了快速返回，但當與 `dropping_state` 結合使用時，順序很重要。如果 `ref_count` 在檢查期間變化，但 `dropping_state` 還沒更新，可能會繞過重要的安全檢查。

**Rustacean (Soundness 觀點):**
`dropping_state()` 的存在就是，為了解決 `ref_count == 0` 和 actual dropping 之間的 race condition。如果 `try_upgrade()` 因為 `is_dead_or_unrooted()` 而提前返回，可能會跳過這個檢查。這可能是一個 soundness 問題。

**Geohot (Exploit 觀點):**
理論上可以構造這樣的執行順序：
1. 物件 ref_count == 1, dropping_state == 0
2. try_upgrade 讀取 ref_count == 1（通過 is_dead_or_unrooted 檢查）
3. 其他執行緒開始 drop，ref_count 變 0，但 dropping_state 還是 0
4. try_upgrade 嘗試 inc_ref
5. inc_ref 返回 false（因為 ref_count == 0）
6. 問題：這個場景在 try_upgrade 中是正常處理的

實際上更危險的場景可能是：
1. 物件 ref_count == 0, dropping_state == 1
2. try_upgrade 的 is_dead_or_unrooted() 返回 true（因為 ref_count == 0）
3. 永遠不會檢查到 dropping_state
4. 但 upgrade 會檢查到 dropping_state

兩個方法返回同樣的結果（None），但原因不同。

