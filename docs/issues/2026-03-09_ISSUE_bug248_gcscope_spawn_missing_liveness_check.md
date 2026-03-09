# [Bug]: GcScope::spawn Missing Object Liveness Validation

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在 track() 與 spawn() 之間發生 GC collection |
| **Severity (嚴重程度)** | Critical | 可能導致 UAF 或存取錯誤物件 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制觸發 race |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcScope::spawn` in `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`GcScope::spawn()` 在建立 AsyncGcHandle 時，只驗證 GC 指針是否在當前執行緒的 heap 中 (`validate_gc_in_current_heap`)，但沒有驗證物件是否仍然存活。

### 預期行為 (Expected Behavior)
`GcScope::spawn()` 應該在建立 handle 前檢查物件的 liveness：
- `has_dead_flag()` - 物件已死
- `dropping_state` - 物件正在被 drop
- `is_under_construction()` - 物件仍在建構中
- `is_allocated()` - slot 未被 sweep 回收

### 實際行為 (Actual Behavior)
只呼叫 `validate_gc_in_current_heap(tracked.ptr as *const u8);` (line 1143)，缺少 liveness 檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/handles/async.rs:1143-1152`：

```rust
validate_gc_in_current_heap(tracked.ptr as *const u8);  // 只驗證 heap 所屬

let slot_ptr = unsafe {
    let slots_ptr = scope.data.block.slots.get() as *mut HandleSlot;
    slots_ptr.add(idx)
};

unsafe {
    (*slot_ptr).set(tracked.ptr);  // BUG: 沒有 liveness 檢查!
}
```

攻擊情境：
1. 使用者呼叫 `scope.track(&gc_object)` 追蹤 GC 物件
2. 在 `spawn()` 呼叫前，GC 收集了該物件 (lazy sweep 回收 slot)
3. 使用者呼叫 `scope.spawn(|handles| ...)`
4. 同一個 slot 可能分配了新物件
5. `spawn()` 建立指向（現在不同的）物件的 `AsyncGcHandle`，未驗證
6. 使用者存取 handle 時會得到 UAF 或錯誤資料

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcScope, Trace};

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let mut scope = GcScope::new();
    
    // Track an object
    let gc = Gc::new(Data { value: 42 });
    scope.track(&gc);
    
    // Force GC to collect the object
    // (Need to setup precise timing for slot reuse)
    
    // Spawn - this creates handle to possibly different object
    scope.spawn(async move |handles| {
        // Access may be to wrong object
    });
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcScope::spawn()` 中，於 `validate_gc_in_current_heap` 後加入 liveness 檢查：

```rust
validate_gc_in_current_heap(tracked.ptr as *const u8);

// Add liveness checks
let header = unsafe { crate::heap::ptr_to_page_header(tracked.ptr) };
if !header.is_allocated(tracked.index) {
    panic!("GcScope::spawn: tracked object was deallocated");
}
if header.has_dead_flag(tracked.index) {
    panic!("GcScope::spawn: tracked object is dead");
}
```

參考類似模式：
- Bug 194: `AsyncGcHandle::downcast_ref` missing is_allocated check
- Bug 195: `Handle::get/to_gc` missing is_allocated check  
- Bug 196: `AsyncHandle::get/to_gc` missing is_allocated check

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題類似於其他 handle 創建路徑的 liveness 驗證缺失。當 slot 被 lazy sweep 回收並重新分配時，舊的追蹤指標可能指向新物件。GcScope 的設計需要確保在 spawn 時所有追蹤的物件仍然是有效的 root。

**Rustacean (Soundness 觀點):**
這是經典的 Use-After-Free 漏洞模式。雖然有 `validate_gc_in_current_heap` 檢查指標是否在正確的 heap 中，但沒有驗證物件是否仍然存在。這可能導致記憶體安全問題。

**Geohot (Exploit 觀點):**
如果攻擊者能控制 GC 時機，可能會：
1. 建立一個即將被回收的物件
2. 觸發 GC 回收該物件
3. 在同一個 slot 分配惡意資料
4. 透過 spawn 建立的 handle 存取錯誤資料
