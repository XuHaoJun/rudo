# [Bug]: Handle::to_gc() 與 Handle::get() 缺少 Generation 檢查導致潛在 Slot Reuse UAF

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在 HandleScope 存活期間觸發 slot sweep 和 reuse |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free 或錯誤的引用計數操作 |
| **Reproducibility (復現難度)** | Very High | 需要精確時序控制，單執行緒難以穩定復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Handle::to_gc()` in `handles/mod.rs:358-399`, `Handle::get()` in `handles/mod.rs:302-326`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`Handle::to_gc()` 應該像 `GcHandle::resolve_impl()` 一樣，在執行 `inc_ref()` 前後檢查 generation，以檢測 slot 是否被 sweep 並重新分配給其他物件。

`Handle::get()` 應該在讀取 `gc_box.value()` 前檢查 generation，以確保讀取的是原始物件而非被 reuse 後的新物件。

### 實際行為 (Actual Behavior)

`Handle::to_gc()` (handles/mod.rs:358-399) 缺少 generation 檢查。雖然有 `is_allocated`、`has_dead_flag`、`dropping_state` 和 `is_under_construction` 檢查，但這些檢查和 `try_inc_ref_if_nonzero()` 調用之間存在 TOCTOU 窗口。如果 slot 在檢查後但在 `inc_ref` 前被 sweep 並 reuse，generation 會改變，導致 `inc_ref` 操作錯誤物件的引用計數。

`Handle::get()` (handles/mod.rs:302-326) 同樣缺少 generation 檢查。如果 slot 在檢查後被 reuse，`gc_box.value()` 可能讀取錯誤物件的記憶體。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `handles/mod.rs`

`GcHandle::resolve_impl()` (lines 234-247) 有正確的 generation 檢查：
```rust
// Get generation BEFORE inc_ref to detect slot reuse (bug347).
let pre_generation = gc_box.generation();

gc_box.inc_ref();

// Verify generation hasn't changed - if slot was reused, this will panic.
assert_eq!(
    pre_generation,
    gc_box.generation(),
    "GcHandle::resolve: slot was reused between pre-check and inc_ref (generation mismatch)"
);
```

但 `Handle::to_gc()` (lines 358-399) 沒有這個檢查：
```rust
// 只有 is_allocated 檢查，沒有 generation 檢查
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Handle::to_gc: slot has been swept and reused"
    );
}
let gc_box = &*gc_box_ptr;
// ... 檢查 dead_flag, dropping_state, is_under_construction ...
if !gc_box.try_inc_ref_if_nonzero() {  // <-- 可能操作錯誤物件！
    panic!("Handle::to_gc: object is being dropped by another thread");
}
```

`is_allocated` 檢查不足以防止 slot reuse TOCTOU（bug347）。Generation 檢查是必要的，因為 generation 在每次 allocation 時遞增，能可靠地檢測 slot 是否被重新分配。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要多執行緒環境或 Miri 來穩定復現。概念驗證：

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::HandleScope;
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = Arc::new(HandleScope::new(&tcb));
    
    // 建立多個 Gc 並建立 Handle
    let gc1 = Gc::new(Data { value: 42 });
    let gc2 = Gc::new(Data { value: 43 });
    let handle1 = scope.handle(&gc1);
    
    // 嘗試觸發 slot reuse：
    // 1. drop gc1 和 handle1
    // 2. 強制 GC 回收 slot
    // 3. 分配新物件到同樣的 slot（generation 會改變）
    // 4. 嘗試在 handle1 上呼叫 to_gc()
    
    // 這會導致在錯誤的物件上執行 inc_ref
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Handle::to_gc()` 中新增 generation 檢查（仿照 `GcHandle::resolve_impl()`）：

```rust
pub fn to_gc(&self) -> Gc<T> {
    unsafe {
        // ... 現有的檢查 ...
        
        let gc_box = &*gc_box_ptr;
        
        // Get generation BEFORE inc_ref to detect slot reuse
        let pre_generation = gc_box.generation();

        if !gc_box.try_inc_ref_if_nonzero() {
            panic!("Handle::to_gc: object is being dropped by another thread");
        }

        // Verify generation hasn't changed - if slot was reused, this will panic.
        assert_eq!(
            pre_generation,
            gc_box.generation(),
            "Handle::to_gc: slot was reused between pre-check and inc_ref (generation mismatch)"
        );
        
        // ... 後續檢查 ...
    }
}
```

在 `Handle::get()` 中新增 generation 檢查：

```rust
pub fn get(&self) -> &T {
    unsafe {
        // ... 現有的檢查 ...
        
        let gc_box = &*gc_box_ptr;
        
        // Get generation BEFORE reading value to detect slot reuse
        let pre_generation = gc_box.generation();
        
        // Read value
        let value = gc_box.value();
        
        // Verify generation hasn't changed
        assert_eq!(
            pre_generation,
            gc_box.generation(),
            "Handle::get: slot was reused between pre-check and value read (generation mismatch)"
        );
        
        value
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Slot reuse 是 GC 系統中的經典問題。Generation 機制是檢測此問題的標準方法。`is_allocated` 只能告訴我們 slot 是否被分配，不能告訴我們是否被分配給**不同的物件**。每次 allocation 時遞增 generation 確保了物件身份的追蹤。

**Rustacean (Soundness 觀點):**
如果 `inc_ref` 在錯誤的物件上執行，會導致：
1. 錯誤物件的引用計數被增加
2. 當正確物件應該被回收時，可能因為錯誤的計數而無法回收
3. 或者錯誤物件的計數過高，導致記憶體洩漏

這明確違反了記憶體安全。

**Geohot (Exploit 攻擊觀點):**
Slot reuse + 引用計數操作錯誤是經典的記憶體腐敗向量。攻擊者可能：
1. 精心控制 slot reuse 的時機
2. 讓 `inc_ref` 操作攻擊者控制的物件
3. 透過錯誤的引用計數實現 use-after-free 或記憶體洩漏

---

## Re-Opened (2026-03-22) — Closed (2026-03-28)

The concern was that `Handle::get` read `gc_box.value()` before verifying generation. **Current code** (`handles/mod.rs` `Handle::get`, and `handles/async.rs` `AsyncHandle::get` / `get_unchecked`) uses this order: `pre_generation` → `try_inc_ref_if_nonzero` → `assert_eq!(pre_generation, gc_box.generation(), …)` → `dec_ref` and post-checks → **`gc_box.value()` last**. Generation is therefore asserted before the payload read. Same pattern applies to `AsyncHandle::get` and `get_unchecked`.

---

## Original Resolution (2026-03-20)

**Outcome:** (Incorrectly marked Fixed)

Added generation checks to detect slot reuse TOCTOU in:
- `Handle::to_gc()` in `handles/mod.rs:358-399`
- `Handle::get()` in `handles/mod.rs:302-326`
- `AsyncHandle::to_gc()` in `handles/async.rs:733-791`
- `AsyncHandle::get()` in `handles/async.rs:608-636`
- `AsyncHandle::get_unchecked()` in `handles/async.rs:677-705`

The fix follows the same pattern as `GcHandle::resolve_impl()` (bug347):
1. Save generation before inc_ref/value read
2. Verify generation unchanged after operation
3. Panic if generation mismatch detected

---

## 相關 Issue

- bug347: GcHandle::resolve_impl is_allocated check insufficient (same root cause)
- bug349: GcHandle::drop dec_ref slot reuse (related)
- bug348: GcHandle::try_resolve_impl missing post-increment is_allocated check (related)
