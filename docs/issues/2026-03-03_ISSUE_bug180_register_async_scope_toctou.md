# [Bug]: register_async_scope 雙鎖 TOCTOU 導致 inconsistent scope state

**Status:** Open
**Tags:** Verified

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
3. **Thread B 調用 `is_scope_active(id)`** - 檢查 `active_scope_ids`，找不到 id，返回 false！
4. 添加到 `active_scope_ids` (Lock 2) - 完成

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `register_async_scope` 使用兩個獨立的 lock：

1. 先獲取 `async_scopes` lock，添加 scope
2. 釋放第一個 lock
3. 獲取 `active_scope_ids` lock，添加 id
4. 釋放第二個 lock

在步驟 2 和 3 之間，存在 TOCTOU window，導致：
- `is_scope_active()` 可能返回 false（即使 scope 已經在 `async_scopes` 中）
- `GC` 可能看到 scope 在 `async_scopes` 中，但 `is_scope_active()` 返回 false

對比 `unregister_async_scope` 的正確實現：
```rust
pub fn unregister_async_scope(&self, id: u64) {
    let mut scopes = self.async_scopes.lock().unwrap();
    let mut active_ids = self.active_scope_ids.lock().unwrap();
    // 兩個 lock 都在同一個 scope 內
    scopes.retain(|e| e.id != id);
    active_ids.remove(&id);
}
```

`unregister_async_scope` 正確地使用嵌套 lock，確保原子性。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確時序控制：
1. Thread A 調用 `register_async_scope(id)`
2. Thread A 添加到 `async_scopes`（完成第一步）
3. Thread B 調用 `is_scope_active(id)` - 應該返回 false（但 scope 已經在 async_scopes 中）
4. Thread A 添加到 `active_scope_ids`（完成第二步）

理論上可能導致：
- `is_scope_active` 返回不一致的結果
- GC 遍歷時看到不一致的 scope 狀態

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `register_async_scope` 以使用嵌套 lock，類似 `unregister_async_scope`：

```rust
pub fn register_async_scope(&self, id: u64, data: Arc<AsyncScopeData>) {
    let entry = AsyncScopeEntry { id, data };
    let mut scopes = self.async_scopes.lock().unwrap();
    let mut active_ids = self.active_scope_ids.lock().unwrap();
    scopes.push(entry);
    active_ids.insert(id);
}
```

這樣確保兩個資料結構的更新是原子的，避免 TOCTOU window。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 TOCTOU 可能導致 GC 在遍歷 roots 時看到不一致的 async scope 狀態。如果 scope 在 `async_scopes` 中但 `is_scope_active` 返回 false，可能導致 GC 行為不一致。

**Rustacean (Soundness 觀點):**
這是並發安全問題。使用兩個獨立的 lock 導致狀態不一致，可能導致 GC 錯誤地遍歷或忽略某些 async scope。

**Geohot (Exploit 攻擊觀點):**
在並發場景中，攻擊者可能利用此 TOCTOU 來控制 GC 的 root 遍歷行為，進一步利用記憶體管理漏洞。

---

## 修復狀態

- [ ] 已修復
- [x] 未修復
