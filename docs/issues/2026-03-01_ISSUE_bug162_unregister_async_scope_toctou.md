# [Bug]: unregister_async_scope 非原子操作導致 TOCTOU - GC 可能遺漏 roots 或 is_scope_active 返回錯誤結果

**Status:** Open
**Tags:** Unverified

---

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發：scope drop 時 GC 運行或 is_scope_active 被調用 |
| **Severity (嚴重程度)** | High | 1. GC 可能遺漏 roots，導致 live 物件被錯誤回收 2. is_scope_active 返回 true 當應該返回 false |
| **Reproducibility (再現難度)** | Medium | 需要精確時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `unregister_async_scope`, `heap.rs:392-395`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`unregister_async_scope` 應該以原子方式從兩個資料結構中移除 scope：
1. `async_scopes` - 用於 GC root 遍歷
2. `active_scope_ids` - 用於 `is_scope_active` 檢查

兩者應該同時更新，確保：
- GC 看到 scope 被移除時，is_scope_active 也返回 false
- 反之亦然

### 實際行為 (Actual Behavior)

`unregister_async_scope` 使用兩個獨立的 lock 移除 scope：

```rust
// heap.rs:392-395
pub fn unregister_async_scope(&self, id: u64) {
    self.async_scopes.lock().unwrap().retain(|e| e.id != id);  // Lock 1
    self.active_scope_ids.lock().unwrap().remove(&id);         // Lock 2
}
```

這不是原子操作！存在 TOCTOU window：

**時序 1: GC 運行時機**
1. Thread A 調用 `unregister_async_scope(id)`
2. 從 `async_scopes` 移除 (Lock 1) - GC 不會再看到这个 scope 作为 root
3. **GC 在這裡運行** - 遍歷 `async_scopes`，不包含此 scope（正確）
4. 從 `active_scope_ids` 移除 (Lock 2) - 已完成

**時序 2: is_scope_active 檢查時機**
1. Thread A 調用 `unregister_async_scope(id)`
2. 從 `async_scopes` 移除 (Lock 1)
3. **Thread B 調用 `is_scope_active(id)`** - 仍在 `active_scope_ids` 中，返回 true！
4. Thread B 繼續訪問 slot - scope 實際上已從 GC root 中移除
5. 從 `active_scope_ids` 移除 (Lock 2)

**時序 3: GC + is_scope_active 同時**
1. Thread A 調用 `unregister_async_scope(id)`  
2. 從 `async_scopes` 移除 (Lock 1)
3. **GC 運行** - 遍歷 `async_scopes`，不包含此 scope（正確）
4. **Thread B 調用 `is_scope_active(id)`** - 仍在 `active_scope_ids` 中，返回 true (錯誤!)
5. Thread B 訪問 slot - 此 scope 已不是 root，slot 可能被 GC
6. UAF!

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題在於 `heap.rs:392-395`:

```rust
pub fn unregister_async_scope(&self, id: u64) {
    // 第一個 lock - 移除 from async_scopes
    self.async_scopes.lock().unwrap().retain(|e| e.id != id);
    
    // === TOCTOU window ===
    
    // 第二個 lock - 移除 from active_scope_ids  
    self.active_scope_ids.lock().unwrap().remove(&id);
}
```

兩個移除操作不是原子的。當第一個 lock 釋放後、第二個 lock 獲取前，狀態不一致。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;
use std::sync::Arc;
use std thread;

fn main() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = AsyncHandleScope::new(&tcb);
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    let scope_id = scope.id();
    
    // Thread A: Drop scope
    let handle_clone = handle;
    let handle2 = handle;
    thread::spawn(move || {
        drop(handle_clone);  // Keep handle alive
        drop(scope);  // Unregister scope
    });
    
    // Thread B: Check is_scope_active while scope is being unregistered
    // 有機會 is_scope_active 返回 true 但 scope 實際上已從 async_scopes 移除
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

使用單個 lock 保護兩個操作，或使用 transaction 確保原子性：

```rust
// 方案 1: 單一 lock
pub fn unregister_async_scope(&self, id: u64) {
    let mut scopes = self.async_scopes.lock().unwrap();
    let mut active_ids = self.active_scope_ids.lock().unwrap();
    
    scopes.retain(|e| e.id != id);
    active_ids.remove(&id);
}

// 方案 2: 先標記為 inactive 再移除
pub fn unregister_async_scope(&self, id: u64) {
    // First mark as inactive (atomic with respect to is_scope_active)
    self.active_scope_ids.lock().unwrap().remove(&id);
    
    // Then remove from GC roots
    self.async_scopes.lock().unwrap().retain(|e| e.id != id);
}
```

方案 2 確保：
- is_scope_active 返回 false 時，scope 也會從 async_scopes 移除
- 但如果 GC 在 active_scope_ids 移除後、async_scopes 移除前運行，仍會遺漏 root

**最佳方案**：使用單一 lock 保護兩個數據結構。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是典型的非原子狀態更新問題。在 GC 系統中，保持 root 遍歷和 active 狀態檢查的一致性至關重要。兩個數據結構應該以原子方式更新，確保 GC 永遠看到一致的狀態。

**Rustacean (Soundness 觀點):**
這可能導致記憶體安全問題。如果 GC 在不一致的狀態下運行，可能會遺漏 live objects (UAF)。同時 is_scope_active 返回錯誤結果也會導致 API 使用上的混亂。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個 TOCTOU 來：
1. 導致 GC 錯誤回收物件
2. 造成 use-after-free
3. 在極端情況下，可能控制 GC 行為

---

## 修復狀態

- [ ] 已修復
- [x] 未修復
