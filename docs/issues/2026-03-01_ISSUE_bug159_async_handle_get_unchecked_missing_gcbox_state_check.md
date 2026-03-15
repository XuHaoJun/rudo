# [Bug]: AsyncHandle::get_unchecked() Missing Safety Checks for GcBox State

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 開發者可能因為效能考量使用 get_unchecked() 但未留意安全要求 |
| **Severity (嚴重程度)** | High | 可能導致 Use-After-Free 或讀取已 drop 的記憶體 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序：scope 活著但 Gc 已被收集 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** AsyncHandle::get_unchecked() in handles/async.rs
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`get_unchecked()` 應該與 `get()` 有相同的安全檢查，但可以省略 scope 活性檢查以提升效能。根據文件說明，它只需要確保 scope 仍然活著。

### 實際行為
`get_unchecked()` 完全沒有檢查 GcBox 的狀態：
- 沒有檢查 `has_dead_flag()`
- 沒有檢查 `dropping_state()`
- 沒有檢查 `is_under_construction()`

而 `get()` 函數（lines 586-591）有完整的檢查。

這導致使用 `get_unchecked()` 可能存取到已回收或正在被 drop 的記憶體，造成未定義行為。

---

## 🔬 根本原因分析

在 `crates/rudo-gc/src/handles/async.rs` 中：

**get() 有完整檢查 (lines 582-593):**
```rust
let gc_box = &*gc_box_ptr;
assert!(
    !gc_box.has_dead_flag()
        && gc_box.dropping_state() == 0
        && !gc_box.is_under_construction(),
    "AsyncHandle::get: cannot access a dead, dropping, or under construction Gc"
);
gc_box.value()
```

**get_unchecked() 缺少檢查 (lines 629-633):**
```rust
pub unsafe fn get_unchecked(&self) -> &T {
    let slot = unsafe { &*self.slot };
    let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
    unsafe { &*gc_box_ptr }.value()  // 直接存取，無任何檢查
}
```

Safety 文件（lines 596-607）只提到 scope 必須活著，但沒有提到呼叫者也必須確保 Gc 沒有死亡/正在drop/建設中。

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
// 此 PoC 展示問題：get_unchecked() 可能存取已回收的記憶體
// 而 get() 會正確 panic

use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;

#[derive(Trace)]
struct Data { value: i32 }

async fn trigger_bug() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = AsyncHandleScope::new(&tcb);
    
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    
    // Drop Gc - 讓物件變成候選回收
    drop(gc);
    
    // 強制 GC 回收
    rudo_gc::collect_full();
    
    // get() 會正確 panic - 檢查了 dead flag
    // handle.get(); // 會 panic
    
    // get_unchecked() 沒有這些檢查，可能造成 UAF
    // 行為未定義 - 可能讀取到垃圾資料
    let _value = unsafe { handle.get_unchecked().value };
}
```

---

## 🛠️ 建議修復方案

有兩個選項：

**選項 1：添加與 get() 相同的檢查**
```rust
pub unsafe fn get_unchecked(&self) -> &T {
    let slot = unsafe { &*self.slot };
    let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
    unsafe {
        let gc_box = &*gc_box_ptr;
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "AsyncHandle::get_unchecked: cannot access a dead, dropping, or under construction Gc"
        );
        gc_box.value()
    }
}
```

**選項 2：更新安全文件說明**
若希望保持 current behavior（不檢查），需要在 safety documentation 中明確說明：
- 呼叫者必須確保 Gc 仍然活著（未死亡、未在 drop 中、非建設中）
- 這是呼叫者的責任，而非 scope 活性

---

## 🗣️ 內部討論紀錄

**R. Kent Dybvig (GC 架構觀點):**
此問題源於 API design 的不一致。get() 與 get_unchecked() 應該有清楚的安全 contract。雖然 scope 活性是必要條件，但 Gc 本身的狀態也同樣重要。當 Gc 被收集後，即使 scope 仍然活著，slot 中的指標也許指向已回收的記憶體。

**Rustacean (Soundness 觀點):**
這是一個 soundness issue。`get_unchecked()` 的 unsafe contract 不完整，文件沒有清楚說明呼叫者需要保證什麼。使用這個 function 的開發者可能會不小心觸發 UB。

**Geohot (Exploit 觀點):**
攻擊者可能利用這個漏洞：若能控制 GC 時機，可在 scope 仍然活著的情況下讓 Gc 被收集，然後讀取已釋放的記憶體（讀取 primitive types 時可能讀到舊資料，若包含指標則可能造成指標混淆）。

---

## Resolution (2026-03-02)

**Outcome:** Fixed (same fix as bug 157).

`AsyncHandle::get_unchecked()` now checks `has_dead_flag()`, `dropping_state()`, and `is_under_construction()` before dereferencing, matching `get()` behavior.
