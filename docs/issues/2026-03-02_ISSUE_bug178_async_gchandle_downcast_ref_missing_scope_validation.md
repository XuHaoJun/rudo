# [Bug]: AsyncGcHandle::downcast_ref 缺少 Scope 驗證 - 與 AsyncHandle::get() 行為不一致

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發場景：scope drop 時另一執行緒正在調用 downcast_ref() |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，記憶體安全問題 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncGcHandle::downcast_ref`, `handles/async.rs:1286-1303`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`AsyncGcHandle::downcast_ref()` 直接存取 `self.slot` 而沒有透過 `with_scope_lock_if_active()` 驗證 scope 是否仍然活躍。這與 `AsyncHandle::get()` 的實現不一致，後者已於 bug148 修復此 TOCTOU 問題。

### 預期行為 (Expected Behavior)

當 scope 已經被 drop 時，`downcast_ref()` 應該檢測到 scope 已失效並返回 `None` 或 panic，不應存取已釋放的記憶體。應該與 `AsyncHandle::get()` 的行為一致。

### 實際行為 (Actual Behavior)

`AsyncGcHandle::downcast_ref()` 直接執行：
```rust
let slot = unsafe { &*self.slot };  // 沒有 scope 驗證！
```

這與 `AsyncHandle::get()` 的正確實現形成對比，後者使用 `with_scope_lock_if_active()` 來確保 scope 在存取期間保持有效。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/async.rs:1286-1303`：

```rust
pub fn downcast_ref<T: Trace + 'static>(&self) -> Option<&T> {
    if self.type_id == TypeId::of::<T>() {
        // BUG: 沒有 scope 驗證！
        let slot = unsafe { &*self.slot };
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        // ...
    }
}
```

對比 `AsyncHandle::get()` (lines 570-605) 的正確實現：
```rust
let gc_box_ptr = tcb
    .with_scope_lock_if_active(self.scope_id, || unsafe {
        (*self.slot).as_ptr() as *const GcBox<T>
    })
    .unwrap_or_else(|| {
        panic!("AsyncHandle used after scope was dropped...")
    });
```

問題在於：
1. `AsyncHandle::get()` 已於 bug148 修復，使用 `with_scope_lock_if_active()` 確保原子性
2. `AsyncGcHandle::downcast_ref()` 沒有套用相同的修復
3. 當 scope 被 drop 時，`self.slot` 指標可能指向已釋放的記憶體

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. 創建一個 `GcScope` 和 `AsyncGcHandle`
2. 在一個執行緒中調用 `downcast_ref()`
3. 在另一個執行緒中 drop scope
4. 可能導致 use-after-free

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

修改 `AsyncGcHandle::downcast_ref()` 以使用 `with_scope_lock_if_active()` 驗證 scope：

```rust
pub fn downcast_ref<T: Trace + 'static>(&self) -> Option<&T> {
    if self.type_id == TypeId::of::<T>() {
        let tcb = crate::heap::current_thread_control_block()
            .expect("AsyncGcHandle::downcast_ref() must be called within a GC thread");

        // 使用 with_scope_lock_if_active 來防止 TOCTOU
        let gc_box_ptr = tcb
            .with_scope_lock_if_active(self.scope_id, || unsafe {
                let slot = &*self.slot;
                slot.as_ptr() as *const GcBox<T>
            })?;

        unsafe {
            let gc_box = &*gc_box_ptr;
            if gc_box.is_under_construction()
                || gc_box.has_dead_flag()
                || gc_box.dropping_state() != 0
            {
                return None;
            }
            Some(gc_box.value())
        }
    } else {
        None
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這與 bug148 修復的 `AsyncHandle::get()` TOCTOU 問題完全相同。當 scope 被 drop 時，`AsyncScopeData`（包含 slot）可以被釋放，導致 use-after-free。這是基本的記憶體安全問題。

**Rustacean (Soundness 觀點):**
直接存取可能已釋放的記憶體是未定義行為 (UB)。需要套用與 `AsyncHandle::get()` 相同的修復模式。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過精確的時序控制來觸發此 use-after-free，進而實現記憶體佈局控制。
