# [Bug]: WeakCrossThreadHandle::is_valid() 在 Origin Thread 終止後返回 false，但 Weak Reference 本身可能仍然有效

**Status:** Invalid
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 origin thread 終止後調用 is_valid() 時觸發 |
| **Severity (嚴重程度)** | Low | 僅影響 API 行為一致性，不導致記憶體錯誤 |
| **Reproducibility (復現難度)** | Medium | 可透過簡單測試復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::is_valid()` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`WeakCrossThreadHandle::is_valid()` 應該在以下情況返回 `true`：
1. Origin thread 仍然存活，且 weak reference 指向的對象仍然 alive
2. Origin thread 已終止，但 weak reference 仍然可以透過 orphan 機制升級

這與 `GcHandle::is_valid()` 的行為一致，後者會檢查 orphan table。

### 實際行為 (Actual Behavior)

`WeakCrossThreadHandle::is_valid()` 當 origin thread 終止後直接返回 `false`，即使 weak reference 本身可能仍然有效。

**代碼位置：** `handles/cross_thread.rs` 第 635-640 行

```rust
pub fn is_valid(&self) -> bool {
    // BUG: 當 origin thread 終止時直接返回 false
    if self.origin_tcb.upgrade().is_none() {
        return false;  // <-- 這裡直接返回 false，沒有檢查 weak 是否仍然有效
    }
    self.weak.is_live()
}
```

### 對比：GcHandle::is_valid() 的正確實現

```rust
pub fn is_valid(&self) -> bool {
    if self.handle_id == HandleId::INVALID {
        return false;
    }
    // 正確：先檢查 orphan table
    let orphan = heap::lock_orphan_roots();
    if orphan.contains_key(&(self.origin_thread, self.handle_id)) {
        return true;  // <-- 在 orphan 中找到，返回 true
    }
    drop(orphan);
    // 然後檢查 TCB
    self.origin_tcb.upgrade().is_some_and(|tcb| {
        let roots = tcb.cross_thread_roots.lock().unwrap();
        roots.strong.contains_key(&self.handle_id)
    })
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`WeakCrossThreadHandle::is_valid()` 的實現邏輯有問題：

1. 當 `origin_tcb.upgrade().is_none()` 時，直接返回 `false`
2. 但這個檢查的目的是確保可以安全地調用 `resolve()`（需要 origin thread）
3. 然而，`is_valid()` 應該只檢查 weak reference 是否有效，而不是檢查 origin thread 是否存活

這導致了行為不一致：
- `WeakCrossThreadHandle::resolve()` 在 origin thread 終止後 panic（這是正確的）
- `WeakCrossThreadHandle::is_valid()` 在 origin thread 終止後返回 false（這可能不是預期行為）
- 但是 `WeakCrossThreadHandle::try_resolve()` 在 origin thread 終止後可以返回 `Some`（如果 reuse ThreadId）

實際上，經過進一步分析，這可能是**設計決策**而非 bug：
- `is_valid()` 的目的是檢查是否可以安全地調用 `resolve()`
- 當 origin thread 終止後，不能調用 `resolve()`（會 panic）
- 所以 `is_valid()` 返回 `false` 是合理的

**但是**，這與 `GcHandle::is_valid()` 的行為不一致，後者即使在 orphan 情況下也返回 `true`。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use std::thread;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc = Gc::new(Data { value: 42 });
    let weak = gc.weak_cross_thread_handle();
    
    // 在 origin thread 上，is_valid() 應該返回 true
    assert!(weak.is_valid(), "Should be valid on origin thread");
    
    // 獲取 weak clone 以便在另一個 thread 上測試
    let weak_clone = weak.clone();
    
    // Spawn 新 thread，嘗試檢查 weak validity
    let handle = thread::spawn(move || {
        // 在新 thread 上，origin thread 已經終止（主 thread 仍在運行）
        // is_valid() 返回 false，但 weak 本身可能仍然有效
        println!("is_valid: {}", weak_clone.is_valid());
        
        // 嘗試 try_resolve - 這可能成功如果 ThreadId 被 reuse
        if let Some(resolved) = weak_clone.try_resolve() {
            println!("try_resolve succeeded: {}", resolved.value);
        } else {
            println!("try_resolve failed");
        }
    });
    
    handle.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

如果要保持與 `GcHandle::is_valid()` 一致的行為：

```rust
pub fn is_valid(&self) -> bool {
    // 先檢查 weak 是否 live（這個檢查是獨立的，不依賴 origin thread）
    if !self.weak.is_live() {
        return false;
    }
    
    // 如果 weak 是 live 的，檢查是否可以調用 resolve()
    // 這是 is_valid() 的主要目的：檢查 resolve() 是否會成功
    if self.origin_tcb.upgrade().is_none() {
        // Origin thread 已終止
        // 如果是因為 ThreadId reuse，可能仍然可以調用 try_resolve()
        // 返回 false 是合理的，因為 resolve() 會 panic
        return false;
    }
    
    // Origin thread 存活，weak 也是 live 的
    true
}
```

或者，明確記錄這是設計決策，與 `GcHandle::is_valid()` 的行為差異：

> 注意：`WeakCrossThreadHandle::is_valid()` 當 origin thread 終止後返回 `false`。
> 這與 `GcHandle::is_valid()` 不同，後者會檢查 orphan table。
> 這是因為 weak reference 的语义不同：它不依賴 registration，而是直接引用 object。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- Weak reference 的有效性不應該依賴於 origin thread 的生命週期
- 只要 underlying object 仍然活著，weak 就應該可以升級
- 但 resolve() 需要 origin thread 是因為 T 可能不是 Send，需要在創建線程上訪問

**Rustacean (Soundness 觀點):**
- 這不是 soundness 問題，是 API 行為一致性问题
- `is_valid()` 的語義需要明確：是檢查 "object 是否活著" 還是 "是否能調用 resolve()"

**Geohot (Exploit 攻擊觀點):**
- 目前沒有發現可以利用的地方
- 這是 API 設計問題，不影響記憶體安全

---

## 進一步分析

經過仔細考慮，這更像是**設計決策問題**而非 bug。讓我重新評估：

1. `is_valid()` 的目的是檢查 `resolve()` 是否會成功
2. 當 origin thread 終止後，`resolve()` 會 panic（因為 thread ID 不匹配）
3. 所以 `is_valid()` 返回 `false` 是合理的

但這與 `GcHandle::is_valid()` 不一致，後者即使在 orphan 情況下也會檢查 registration。

**可能的 bug**: 應該在 origin thread 終止後檢查是否可以通过 try_resolve() 成功（如果 ThreadId 被 reuse）。

---

## Resolution

需要決定：
1. 這是 bug 還是設計決策？
2. 如果是 bug，修復方案是什麼？
3. 是否需要記錄這個行為差異？

---

## Resolution (2026-03-21)

**Outcome:** Invalid — reported inconsistency does not match current code or API contract.

**Verification:**

1. **`WeakCrossThreadHandle::is_valid()`** (`handles/cross_thread.rs`) is documented as returning `true` only when **`resolve()` / `try_resolve()`** could succeed; it intentionally requires a live origin TCB and `weak.is_live()`.

2. **`try_resolve()`** begins with `self.origin_tcb.upgrade()?`, so when the origin thread has terminated it always returns `None` — the PoC claim that ThreadId reuse could make `try_resolve()` succeed is **outdated**; TCB liveness is checked before any `ThreadId` comparison (same pattern as `resolve` / `try_upgrade`).

3. **`GcHandle`** uses the orphan table and `handle_id`; **`WeakCrossThreadHandle`** holds only `GcBoxWeakRef` + origin metadata and does not participate in orphan registration. Parity with `GcHandle::is_valid()` is therefore not applicable — different root/registration model.

No code change: behavior matches documented semantics and matches `try_resolve()`.
