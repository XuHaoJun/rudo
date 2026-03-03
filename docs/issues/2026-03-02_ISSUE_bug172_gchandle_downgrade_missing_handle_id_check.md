# [Bug]: GcHandle::downgrade() Missing handle_id INVALID Check

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需在 handle unregistered 後呼叫 downgrade |
| **Severity (嚴重程度)** | Medium | API 不一致，可能導致預期外的行為 |
| **Reproducibility (復現難度)** | Low | 容易重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade()` (cross_thread.rs:290-306)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::downgrade()` 應該與 `resolve()`, `try_resolve()`, 和 `clone()` 具有一致的行為，在處理已註銷的 handle 時進行相同的檢查。

### 實際行為 (Actual Behavior)

- `GcHandle::resolve()` (cross_thread.rs:167-170) 正確檢查 `handle_id == HandleId::INVALID`
- `GcHandle::try_resolve()` (cross_thread.rs:241-243) 正確檢查 `handle_id == HandleId::INVALID`
- `GcHandle::clone()` (cross_thread.rs:313-315) 正確檢查 `handle_id == HandleId::INVALID`
- 但 `GcHandle::downgrade()` (cross_thread.rs:290-306) **缺少**此檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/handles/cross_thread.rs:290-306`:

```rust
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle"
        );
        gc_box.inc_weak();
    }
    // MISSING: 沒有檢查 handle_id == HandleId::INVALID
    WeakCrossThreadHandle { ... }
}
```

對比 `GcHandle::resolve()`:
```rust
pub fn resolve(&self) -> Gc<T> {
    assert!(
        self.handle_id != HandleId::INVALID,  // <-- 有這個檢查
        "GcHandle::resolve: handle has been unregistered"
    );
    // ...
}
```

這導致 API 不一致：
- 使用者可能會對已註銷的 handle 呼叫 downgrade()
- 其他方法會 panic，但 downgrade 會繼續執行

雖然底層的 GcBox 檢查 (has_dead_flag, dropping_state, is_under_construction) 可能會捕獲大多的情況，但缺少 handle_id 檢查是 API 層面的不一致。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::GcHandle;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let gc: Gc<Data> = Gc::new(Data { value: 42 });
    let mut handle = gc.cross_thread_handle();
    
    // Unregister the handle
    handle.unregister();
    
    // This should panic like resolve()/try_resolve()/clone()
    // But currently it doesn't check handle_id
    let _weak = handle.downgrade();  // Inconsistent behavior!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcHandle::downgrade()` 開頭新增 `handle_id == HandleId::INVALID` 檢查：

```rust
pub fn downgrade(&self) -> WeakCrossThreadHandle<T> {
    assert!(
        self.handle_id != HandleId::INVALID,
        "GcHandle::downgrade: handle has been unregistered"
    );
    
    unsafe {
        let gc_box = &*self.ptr.as_ptr();
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "GcHandle::downgrade: cannot downgrade a dead, dropping, or under construction GcHandle"
        );
        gc_box.inc_weak();
    }
    // ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 API 一致性問題。所有處理 GcHandle 的方法應該對已註銷的 handle 有一致的行為。雖然底層的 GcBox 檢查可能會捕獲大多的情況，但明確檢查 handle_id 可以提供更好的錯誤訊息和 API 一致性。

**Rustacean (Soundness 觀點):**
這不是一個 soundness 問題，但是一個 API 設計問題。缺少檢查會導致不一致的行為，可能讓使用者感到困惑。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個不一致性來繞過某些檢查。雖然不太可能直接導致安全問題，但 API 不一致可能在某些情況下被利用。

---

## Resolution (2026-03-03)

**Fixed.** The `handle_id != HandleId::INVALID` check was added to `GcHandle::downgrade()` in `cross_thread.rs` (lines 293–296). The test `test_downgrade_unregistered_handle_panics` in `cross_thread_handle.rs` verifies that downgrading an unregistered handle panics with "cannot downgrade an unregistered GcHandle".
