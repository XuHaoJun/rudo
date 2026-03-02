# [Bug]: register_async_scope 雙鎖 TOCTOU 導致 inconsistent scope state

**Status:** Open
**Tags:** Unverified

---

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發：scope 註冊時 GC 運行或 is_scope_active 被調用 |
| **Severity (嚴重程度)** | High | 1. GC 可能看到不一致的 scope 狀態 2. is_scope_active 返回錯誤結果 |
| **Reproducibility (復現難度)** | Medium | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `register_async_scope`, `heap.rs:395-399`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`register_async_scope` 應該以原子方式將 scope 添加到兩個資料結構中：
1. `async_scopes` - 用於 GC root 遍歷
2. `active_scope_ids` - 用於 `is_scope_active` 檢查

兩者應該同時更新，確保：
- GC 看到 scope 被添加時，is_scope_active 也返回 true
- 反之亦然

### 實際行為 (Actual Behavior)

`register_async_scope` 使用兩個獨立的 lock 添加 scope：

```rust
// heap.rs:395-399
pub fn register_async_scope(&self, id: u64, data: Arc<AsyncScopeData>) {
    let entry = AsyncScopeEntry { id, data };
    self.async_scopes.lock().unwrap().push(entry);       // Lock 1
    self.active_scope_ids.lock().unwrap().insert(id);    // Lock 2
}
```

這不是原子操作！存在 TOCTOU window：

**時序 1: GC 運行時機**
1. Thread A 調用 `register_async_scope(id)`
2. 添加到 `async_scopes` (Lock 1) - GC 會看到这个 scope 作为 root
3. **GC 在這裡運行** - 遍歷 `async_scopes`，包含此 scope（正確）
4. 添加到 `active_scope_ids` (Lock 2) - 已完成

**時序 2: is_scope_active 檢查時機**
1. Thread A 調用 `register_async_scope(id)`
2. 添加到 `async_scopes` (Lock 1)
3. **Thread B 調用 `is_scope_active(id)`** - 不在 active_scope_ids 中，返回 false！
4. Thread B 放棄訪問 slot - 但 scope 實際上已註冊為 GC root
5. 添加到 `active_scope_ids` (Lock 2)

**時序 3: 與 unregister 並發**
1. Thread A 調用 `register_async_scope(id)` - 添加到 async_scopes
2. Thread B 調用 `unregister_async_scope(id)` - 從 async_scopes 移除
3. **GC 運行** - 可能看到不一致狀態
4. Thread B 從 active_scope_ids 移除

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `heap.rs:395-399`:

```rust
pub fn register_async_scope(&self, id: u64, data: Arc<AsyncScopeData>) {
    let entry = AsyncScopeEntry { id, data };
    self.async_scopes.lock().unwrap().push(entry);  // Lock 1
    
    // === TOCTOU window ===
    
    self.active_scope_ids.lock().unwrap().insert(id); // Lock 2
}
```

兩個添加操作不是原子的。當第一個 lock 釋放後、第二個 lock 獲取前，狀態不一致。

此問題與 bug162 相同，但影響不同的 API 函數。`unregister_async_scope` 已經修復為使用單一 lock，但 `register_async_scope` 遺漏了相同的修復。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn main() {
    // 需要多執行緒精確時序控制才能穩定重現
    // 此為概念驗證
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

參考 `unregister_async_scope` 的修復模式，使用單一 lock 保護兩個資料結構：

```rust
pub fn register_async_scope(&self, id: u64, data: Arc<AsyncScopeData>) {
    let entry = AsyncScopeEntry { id, data };
    let mut scopes = self.async_scopes.lock().unwrap();
    let mut active_ids = self.active_scope_ids.lock().unwrap();
    
    scopes.push(entry);
    active_ids.insert(id);
}
```

這確保了：
1. 添加操作是原子的
2. 與 `unregister_async_scope` 的鎖定順序一致
3. 避免 TOCTOU 窗口

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題影響 async scope 的 root 追蹤一致性。如果 GC 在不一致狀態下運行，可能導致：
- 遺漏 live 物件（scope 已添加到 async_scopes 但不在 active_scope_ids）
- 或者 scope 應該是 root 但未被遍歷

**Rustacean (Soundness 觀點):**
這是經典的 TOCTOU 漏洞，與之前修復的 bug162 模式相同。雖然不會導致傳統意義的 UB，但可能導致記憶體錯誤（使用已回收的物件）。

**Geohot (Exploit 觀點):**
若攻擊者能控制 scope 註冊和 GC 時序，可能：
- 誘使 GC 遺漏 live 物件，導致 early collection
- 或讓 is_scope_active 返回錯誤結果，導致 use-after-free
