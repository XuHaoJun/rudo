# [Bug]: Ephemeron Trace 實現使用 is_key_alive() 有 TOCTOU 漏洞，與 GcCapture 實現不一致

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 GC marking 期間 key 死亡 |
| **Severity (嚴重程度)** | Medium | 可能導致 key 的 GC 指標未被正確追蹤 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Trace` implementation for `Ephemeron<K, V>` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`Ephemeron<K, V>` 的 `Trace` 實現使用 `is_key_alive()` 來檢查 key 是否存活，但這有 TOCTOU (Time-of-Check-Time-of-Use) 漏洞。與此同時，`GcCapture` 的實現正確地使用 `try_upgrade()` 來避免 TOCTOU。

### 預期行為 (Expected Behavior)
`Trace` 實現應該使用與 `GcCapture` 實現相同的模式來避免 TOCTOU。

### 實際行為 (Actual Behavior)
`Trace` 實現使用 `is_key_alive()`，而 `GcCapture` 使用 `try_upgrade()`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs` 的 `Trace for Ephemeron` 實現中 (lines 2483-2492):

```rust
unsafe impl<K: Trace + 'static, V: Trace + 'static> Trace for Ephemeron<K, V> {
    fn trace(&self, visitor: &mut impl Visitor) {
        // 問題：使用 is_key_alive() 有 TOCTOU 漏洞
        if self.is_key_alive() {
            visitor.visit(&self.value);
        }
    }
}
```

但在 `GcCapture for Ephemeron` 實現中 (lines 2525-2539)，正確地使用 `try_upgrade()`:

```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    // 正確：使用 try_upgrade() 避免 TOCTOU
    if let Some(key_gc) = self.key.try_upgrade() {
        key_gc.capture_gc_ptrs_into(ptrs);
        self.value.capture_gc_ptrs_into(ptrs);
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要在以下時序下觸發：
1. 創建 Ephemeron，key 和 value 都是 GC 對象
2. 在 GC marking 開始時，key 仍然是活的
3. 在 `is_key_alive()` 返回 true 之後，但在 `visitor.visit()` 之前，key 被標記為死亡
4. 這會導致 value 的 GC 指針未被正確追蹤

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `Trace for Ephemeron` 實現改為使用 `try_upgrade()` 模式，與 `GcCapture` 實現一致：

```rust
unsafe impl<K: Trace + 'static, V: Trace + 'static> Trace for Ephemeron<K, V> {
    fn trace(&self, visitor: &mut impl Visitor) {
        // 使用 try_upgrade() 來避免 TOCTOU，與 GcCapture 實現一致
        if let Some(key_gc) = self.key.try_upgrade() {
            // Key 是活的，我們持有強引用。在這個調用期間 key 保持存活。
            // 只追蹤 key 和 value 的 GC 指針
            key_gc.trace(visitor);
            self.value.trace(visitor);
        }
        // 如果 key 死亡，value 可以被回收 - 不追蹤任何東西（臨時引用語義）
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Ephemeron 的核心語義是「當 key 存活時，value 才應該可達」。TOCTOU 破壞了這個不變性，可能導致 value 在 key 無效後仍然被錯誤地標記為可達。這與 bug106 和 bug122 是同樣的 TOCTOU 模式。

**Rustacean (Soundness 觀點):**
`is_key_alive()` 內部調用 `upgrade()` 使用原子操作來避免 TOCTOU，但 `Trace` 實現直接調用 `is_key_alive()` 而不獲取強引用，導致在檢查和使用之間有窗口期。

**Geohot (Exploit 觀點):**
如果攻擊者能控制 GC 時序，可能利用這個 TOCTOU 窗口來導致不正確的 GC 行為。

---

## 🔗 相關 Issue

- bug106: Ephemeron::upgrade() TOCTOU (已修復)
- bug122: Ephemeron GcCapture TOCTOU (已修復)
