# [Bug]: GcBoxWeakRef::upgrade/try_upgrade 缺少 ref_count > 0 路徑的 CAS 後檢查導致 TOCTOU UAF

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需在 upgrade 過程中有 concurrent drop 發生，且 ref_count 為 1 |
| **Severity (嚴重程度)** | `Critical` | 可能導致 UAF |
| **Reproducibility (復現難度)** | `Medium` | 需特定時序才能觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBoxWeakRef::upgrade()`, `GcBoxWeakRef::try_upgrade()` (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為
`GcBoxWeakRef::upgrade()` 和 `GcBoxWeakRef::try_upgrade()` 應該與 `Weak::upgrade()` 和 `Weak::try_upgrade()` 具有相同的安全檢查。在 `try_inc_ref_if_nonzero()` 成功後，需要第二次檢查 `dropping_state` 和 `has_dead_flag` 以防止 TOCTOU race。

### 實際行為
- `Weak::upgrade()` (ptr.rs:1774-1784) 正確地包含 CAS 後的第二次檢查
- `Weak::try_upgrade()` (ptr.rs:1863-1869) 正確地包含 CAS 後的第二次檢查
- 但 `GcBoxWeakRef::upgrade()` (ptr.rs:505-513) 和 `GcBoxWeakRef::try_upgrade()` (ptr.rs:631-640) 缺少此檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `GcBoxWeakRef::upgrade()` (lines 505-513) 中：
```rust
// ref_count > 0: use atomic try_inc_ref_if_nonzero to avoid TOCTOU with
// concurrent dec_ref (another thread could drop last ref between check and inc_ref)
if !gc_box.try_inc_ref_if_nonzero() {
    return None;
}
Some(Gc {
    ptr: AtomicNullable::new(ptr),
    _marker: PhantomData,
})
```

問題：`try_inc_ref_if_nonzero()` 成功後（ref_count 當時 > 0），沒有檢查 `dropping_state` 和 `has_dead_flag`。

Race 條件：
1. Thread A: 讀取 ref_count = 1, dropping_state = 0
2. Thread B: 讀取 ref_count = 1，呼叫 try_mark_dropping() 成功
3. Thread B: 設置 DEAD_FLAG 並調用 drop_fn（此時 ref_count 仍為 1！）
4. Thread A: try_inc_ref_if_nonzero() CAS 成功 (1->2)
5. Thread A: 返回 Some(Gc) 到已 drop 的物件

根據 `GcBox::dec_ref()` (ptr.rs:167-178) 的實作：
- 當 ref_count == 1 時，dec_ref 會先標記 dropping_state，然後直接調用 drop_fn
- drop_fn 不會減少 ref_count（因為已經是最後一個引用）
- 因此在 drop_fn 執行期間，ref_count 仍然是 1！

這與 `Weak::upgrade` 的 post-CAS 檢查所要防止的 race 完全相同。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcHandle, Trace, collect_full};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct Data {
    value: Arc<AtomicBool>,
}

static START_DROP: AtomicBool = AtomicBool::new(false);

fn main() {
    let value = Arc::new(AtomicBool::new(false));
    let handle: GcHandle<Data> = Gc::new(Data { value: value.clone() });
    
    // 在另一個 thread 啟動 drop
    let handle_clone = handle.clone();
    let handle_clone2 = handle.clone();
    let handle_clone3 = handle.clone();
    
    let handle2 = thread::spawn(move || {
        while !START_DROP.load(Ordering::Relaxed) {}
        drop(handle_clone);
        drop(handle_clone2);
        // 當 ref_count 降至 1 時，drop 會開始
    });
    
    // 開始 drop
    START_DROP.store(true, Ordering::Relaxed);
    
    // 同時嘗試 upgrade - 可能會在 drop 過程中讀取
    // 由於缺少第二次檢查，可能會 UAF
    // GcBoxWeakRef::upgrade() 被 GcHandle::resolve() 內部調用
    let _ = handle_clone3.resolve();
    
    handle2.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcBoxWeakRef::upgrade()` 和 `GcBoxWeakRef::try_upgrade()` 中，於 `try_inc_ref_if_nonzero()` 成功後添加第二次檢查：

```rust
// ref_count > 0: use atomic try_inc_ref_if_nonzero to avoid TOCTOU with
// concurrent dec_ref
if !gc_box.try_inc_ref_if_nonzero() {
    return None;
}

// Post-CAS safety check: verify object wasn't dropped between check and CAS
// (same pattern as Weak::upgrade and Weak::try_upgrade)
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(ptr.as_ptr());
    return None;
}

Some(Gc {
    ptr: AtomicNullable::new(ptr),
    _marker: PhantomData,
})
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`Weak::upgrade` 已有正確的 double-check 模式（防止 resurrection TOCTOU）。`GcBoxWeakRef` 是內部實現，應該與公開 API 具有相同的安全檢查。兩個方法都使用 `try_inc_ref_if_nonzero()` 來避免 ref_count 為 0 時的 race，但沒有防止 ref_count 為 1 時的 dropping race。

**Rustacean (Soundness 觀點):**
缺少第二次檢查可能導致 UAF：物件在 try_inc_ref_if_nonzero() 成功後、CAS 過程中開始 drop，但 ref_count 仍 > 0，導致返回一個指向已釋放記憶體的 Gc。這與 bug168 (Weak::try_upgrade missing post-CAS check) 類似，但是發生在內部的 GcBoxWeakRef 實現。

**Geohot (Exploit 觀點):**
這是一個經典的 TOCTOU 漏洞。與 Weak::try_upgrade 的 bug (bug168) 不同，這個漏洞影響的是內部的 GcBoxWeakRef，可能會被 GcHandle 和 CrossThreadHandle 的使用者觸發。攻擊者可以透過精確時序控制，在 upgrade 過程中觸發 concurrent drop，導致 use-after-free。

---

## Resolution (2026-03-03)

The fix is already present in the codebase. Both `GcBoxWeakRef::upgrade()` (ptr.rs:546–551) and `GcBoxWeakRef::try_upgrade()` (ptr.rs:678–683) include the post-CAS safety check after `try_inc_ref_if_nonzero()`:

```rust
if gc_box.dropping_state() != 0 || gc_box.has_dead_flag() {
    GcBox::dec_ref(ptr.as_ptr());
    return None;
}
```

Full test suite passes. Marked as Fixed.
