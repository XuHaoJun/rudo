# [Bug]: GcHandle::is_valid() 未驗證 Root 存在性 - TOCTOU 導致 Resolve 可能失敗

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要並發場景：線程A調用is_valid()和線程B調用unregister()的時序配合 |
| **Severity (嚴重程度)** | Low | 不會導致記憶體錯誤，但會導致 API 使用不一致，resolve() 可能意外失敗 |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::is_valid()`, `handles/cross_thread.rs:94-96`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`is_valid()` 應該返回 true only 當 handle 確實可用（handle_id 有效且 handle 仍在 root 列表中）。這與 `resolve()` 的行為一致，後者會檢查 both handle_id AND root 列表中的存在性。

### 實際行為 (Actual Behavior)

`is_valid()` 僅檢查 `handle_id != HandleId::INVALID`，但 NOT 檢查 handle 是否仍在 root 列表中。這導致：

1. `is_valid()` 返回 true（因為 handle_id 有效）
2. 但 handle 已被另一個線程從 root 列表中移除（例如，通過另一個 clone 的 drop）
3. 調用 `resolve()` 會失敗並顯示 "handle has been unregistered"

這是一個 TOCTOU（Time-of-check to time-of-use）bug。

### 對比 `resolve()` 的行為

`resolve()` 實現了正確的檢查（`handles/cross_thread.rs:161-166`）：

```rust
// Hold lock during check+use to prevent TOCTOU with unregister.
if let Some(tcb) = self.origin_tcb.upgrade() {
    let roots = tcb.cross_thread_roots.lock().unwrap();
    if !roots.strong.contains_key(&self.handle_id) {
        panic!("GcHandle::resolve: handle has been unregistered");
    }
    // ...
}
```

但 `is_valid()` 沒有這個檢查，導致不一致。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `handles/cross_thread.rs:94-96`：

```rust
pub fn is_valid(&self) -> bool {
    self.handle_id != HandleId::INVALID
}
```

這個實現有兩個問題：

1. **不完整檢查**：只檢查 handle_id，沒有驗證 handle 是否在 root 列表中
2. **API 不一致**：與 `resolve()` 的行為不一致，後者會檢查 both conditions

攻擊場景：
- 線程 A 調用 `is_valid()` 返回 true
- 線程 B（在同一進程中）調用 `unregister()` 移除 handle
- 線程 A 調用 `resolve()` 失敗

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let handle = Arc::new(gc.cross_thread_handle());
    let is_valid_result = Arc::new(AtomicBool::new(false));
    let resolve_panicked = Arc::new(AtomicBool::new(false));

    // 線程 A: 檢查 is_valid
    let handle_a = Arc::clone(&handle);
    let is_valid_result_a = Arc::clone(&is_valid_result);
    let thread_a = thread::spawn(move || {
        let result = handle_a.is_valid();
        is_valid_result_a.store(result, Ordering::SeqCst);
        println!("Thread A: is_valid() = {}", result);
    });

    // 線程 B: unregister handle  
    let handle_b = Arc::clone(&handle);
    let thread_b = thread::spawn(move || {
        // 等待線程 A 開始
        thread::yield_now();
        println!("Thread B: unregistering handle");
        // 注意：unregister 需要 &mut self，所以我們需要用其他方式觸發
        // 這個 PoC 需要修改來實際觸發
    });

    thread_a.join().unwrap();
    thread_b.join().unwrap();
    
    // 注意：由於 is_valid 返回 true 但 resolve 可能會失敗，
    // 這會導致不一致的行為
}
```

實際上，由於 `unregister()` 需要 `&mut self`，這個 TOCTOU 更難觸發。但理論上：
1. 兩個 GcHandle 指向同一個 root
2. 第一個 handle 調用 is_valid()（返回 true）
3. 第二個 handle 被 drop（觸發 unregister）
4. 第一個 handle 調用 resolve()（panic）

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `is_valid()` 以檢查 root 列表中的存在性，與 `resolve()` 的行為一致：

```rust
pub fn is_valid(&self) -> bool {
    if self.handle_id == HandleId::INVALID {
        return false;
    }
    
    // 檢查 handle 是否仍在 root 列表中
    if let Some(tcb) = self.origin_tcb.upgrade() {
        let roots = tcb.cross_thread_roots.lock().unwrap();
        roots.strong.contains_key(&self.handle_id)
    } else {
        let orphan = heap::lock_orphan_roots();
        orphan.contains_key(&(self.origin_thread, self.handle_id))
    }
}
```

注意：這會引入額外的鎖開銷。另一個選項是文檔化這個 TOCTOU 限制，並在 API 文檔中說明 is_valid() 可能返回 true 但 resolve() 仍可能失敗。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 API 設計不一致的問題。is_valid() 應該提供與 resolve() 一樣的安全保證，否則會誤導用戶。以 GC 角度，這不影響 GC 正確性，但影響 API 可用性。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題，不會導致記憶體錯誤。但這是一個 API 可用性問題，可能導致用戶困惑。

**Geohot (Exploit 攻擊觀點):**
目前不可利用。由於 unregister() 需要 &mut self，很難從外部觸發這個 TOCTOU。潛在的利用場景需要精確的時序控制。

---

## 備註

此問題與 bug127（GcHandle::clone 缺少執行緒檢查）為不同類型的問題。這個是 API 一致性問題，那個是執行緒安全問題。
