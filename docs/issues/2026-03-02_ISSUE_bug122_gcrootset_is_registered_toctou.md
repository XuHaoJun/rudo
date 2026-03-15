# [Bug]: GcRootSet::is_registered 存在 TOCTOU Race Condition

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要多執行緒並髮操作才能觸發 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，讀取已釋放記憶體 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒無法復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRootSet::is_registered()`, `tokio/root.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`is_registered()` 應該返回指標在呼叫瞬間是否已註冊。由於此函數用於決定是否將指標視為 GC root，回傳值應該準確反映當前狀態。

### 實際行為 (Actual Behavior)

存在 **TOCTOU (Time-of-Check-Time-of-Use)** 競爭視窗：

1. Thread A: `is_registered(ptr)` 獲取鎖、檢查 `roots.contains(&ptr)`、釋放鎖
2. Thread B: 對同一指標呼叫 `unregister()`，從 roots 移除
3. Thread A: `is_registered()` 回傳 `true`，但實際上指標已不再是 root

呼叫端可能根據這個過時的 `true` 結果，繼續將指標視為受保護的 GC root，導致 use-after-free。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `crates/rudo-gc/src/tokio/root.rs` 第 176-179 行

```rust
#[inline]
pub fn is_registered(&self, ptr: usize) -> bool {
    let roots = self.roots.lock().unwrap();
    roots.contains(&ptr)
}
```

**問題：** 鎖在函數返回前釋放，導致檢查和使用之間存在競爭視窗。根據函數文檔（第 169-179 行），此函數用於檢查「指標是否已註冊」，但釋放鎖後無法保證回傳值在返回時仍然有效。

**對比：** 同檔案中的 `register()` 和 `unregister()` 函數在整個操作過程中持有鎖，確保原子性。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要使用 ThreadSanitizer 或精心設計的時序來觸發。單執行緒無法可靠復現此問題。

概念驗證（需要多執行緒）：
```rust
use rudo_gc::tokio::GcRootSet;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn main() {
    let root_set = GcRootSet::global();
    let ptr = 0x12345678; // 模擬指標
    
    // 註冊指標
    root_set.register(ptr);
    
    let ready = Arc::new(AtomicBool::new(false));
    let ready_clone = ready.clone();
    
    // Thread A: 不斷呼叫 is_registered()
    let set_a = unsafe { std::ptr::read(&root_set) };
    let t1 = thread::spawn(move || {
        while !ready_clone.load(Ordering::Relaxed) {
            thread::yield();
        }
        for _ in 0..10000 {
            let result = set_a.is_registered(ptr);
            if !result {
                // 檢測到競爭：原本應該回傳 true
                println!("Race detected: is_registered returned false");
            }
        }
    });
    
    // Thread B: 不斷呼叫 unregister() 然後重新 register()
    let mut set_b = unsafe { std::ptr::read(&root_set) };
    let t2 = thread::spawn(move || {
        ready.store(true, Ordering::Relaxed);
        for _ in 0..10000 {
            set_b.unregister(ptr);
            set_b.register(ptr); // 快速重建以產生競爭
        }
    });
    
    t1.join().unwrap();
    t2.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. **方案一**：使 `is_registered` 返回一個 ScopeGuard，持有鎖直到 scope 結束：
   ```rust
   pub fn is_registered(&self, ptr: usize) -> ScopeGuard<'_, bool> {
       let roots = self.roots.lock().unwrap();
       ScopeGuard::new(roots, roots.contains(&ptr))
   }
   ```

2. **方案二**：回傳當時的鎖，讓呼叫端決定何時釋放：
   ```rust
   pub fn is_registered(&self, ptr: usize) -> (MutexGuard<'_, HashSet<usize>>, bool) {
       let roots = self.roots.lock().unwrap();
       (roots, roots.contains(&ptr))
   }
   ```

3. **方案三**：將 `is_registered` 標記為 `unsafe`，並在文檔中說明呼叫端必須確保無競爭：
   ```rust
   /// # Safety
   /// 呼叫端必須確保在檢查和使用之間沒有其他執行緒呼叫 unregister()
   pub unsafe fn is_registered(&self, ptr: usize) -> bool {
       ...
   }
   ```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU 競爭條件。GC root table 是物件存活的根本保障。當 `is_registered()` 回傳 `true` 但指標實際已從 root table 移除時，呼叫端會錯誤地假設物件受保護，導致 GC 可能錯誤回收該物件。這與 bug83 類似的 TOCTOU 問題，但發生在不同的 API 層面。

**Rustacean (Soundness 觀點):**
這不是傳統意義上的 undefined behavior，因為不會直接造成記憶體不安全操作。然而，這是一個邏輯錯誤，會導致 GC 行為異常。根據函數的 API 契約，回傳值應該準確反映狀態，而非提供「可能過時」的資訊。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以透過精確時序控制：
1. 建立 GcRootSet 並註冊指標
2. 使用時序技巧讓 `is_registered()` 和 `unregister()` 競爭
3. 利用錯誤的 root 狀態資訊進行進一步利用

此問題雖然不如 GcHandle resolve TOCTOU 嚴重，但仍然是並髮 GC 系統中的潛在漏洞。

---

## Resolution (2026-03-03)

**Outcome:** Fixed.

Implemented Option 3 from the suggested fixes: marked `is_registered` as `unsafe` and added documentation for the TOCTOU race. The caller must ensure that between the call and any use of the result, no other thread calls `unregister(ptr)` on the same pointer. In single-threaded contexts (e.g., tests) this is trivially satisfied.

Changes:
- `GcRootSet::is_registered` is now `pub unsafe fn` with a `# Safety` section documenting the TOCTOU invariant
- Added `# TOCTOU race` section explaining the lock-release-before-return behavior
- Updated all call sites (root.rs tests, tokio_integration.rs) to use `unsafe { set.is_registered(ptr) }`

Production GC code uses `snapshot()` (which holds the lock during the entire operation), not `is_registered`. The fix documents the limitation for any future concurrent use.
