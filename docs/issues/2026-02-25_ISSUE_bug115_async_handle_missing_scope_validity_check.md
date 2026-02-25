# [Bug]: AsyncHandle 缺少 scope 有效性檢查導致 use-after-free

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 在 release 構建中，scope drop 後使用 handle 就會觸發 |
| **Severity (嚴重程度)** | High | 導致 use-after-free，記憶體不安全 |
| **Reproducibility (復現難度)** | Low | 易於重現：scope drop 後調用 get() 或 to_gc() |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle`, `AsyncHandleScope`, `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`AsyncHandle` 的 `get()` 和 `to_gc()` 方法缺少 scope 有效性檢查，導致在 scope drop 後仍可訪問記憶體，造成 use-after-free。

### 預期行為
- `get()` 應該在 debug 和 release 構建中都檢查 scope 是否仍然有效
- `to_gc()` 應該在調用時檢查 scope 是否仍然有效

### 實際行為
1. **`get()` 方法 (line 570-597)**:
   - Debug 構建：檢查 `tcb.is_scope_active(self.scope_id)` (lines 574-583)
   - **Release 構建：完全跳過 scope 有效性檢查！** (`#[cfg(debug_assertions)]` 區塊被編譯掉)

2. **`to_gc()` 方法 (line 671-684)**:
   - **Debug 和 Release 構建都沒有檢查 scope 有效性！**

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/handles/async.rs`:

### Bug 1: get() 在 release 構建中缺少檢查
```rust
// Line 574-583: 僅在 debug 構建中檢查
#[cfg(debug_assertions)]
{
    if !tcb.is_scope_active(self.scope_id) {
        panic!(...);
    }
}

// Line 585-596: 直接訪問 slot，無 scope 有效性保護
let slot = unsafe { &*self.slot };
let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
```

### Bug 2: to_gc() 完全缺少檢查
```rust
// Line 671-684: 沒有任何 scope 有效性檢查
pub fn to_gc(self) -> Gc<T> {
    unsafe {
        let gc_box_ptr = (*self.slot).as_ptr() as *const GcBox<T>;
        // 直接訪問，無保護！
```

根本原因：
1. `get()` 使用 `#[cfg(debug_assertions)]` 條件編譯，導致 release 構建中缺少關鍵檢查
2. `to_gc()` 壓根沒有添加 scope 有效性檢查

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;

#[derive(Trace)]
struct Data { value: i32 }

async fn bug_demo() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    
    // 創建 scope
    let scope = AsyncHandleScope::new(&tcb);
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    
    // Drop scope
    drop(scope);
    
    // BUG: 在 release 構建中，這裡不會 panic，會訪問已釋放的記憶體！
    // Debug 構建會正確 panic
    println!("{}", handle.get().value);
}

async fn bug_demo_to_gc() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    
    let scope = AsyncHandleScope::new(&tcb);
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    
    drop(scope);
    
    // BUG: 這裡無論 debug 還是 release 都會 access 已釋放的記憶體！
    let gc_out = handle.to_gc(); // UAF!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 修復 get():
移除 `#[cfg(debug_assertions)]` 條件，使 scope 檢查在所有構建中都生效：

```rust
pub fn get(&self) -> &T {
    let tcb = crate::heap::current_thread_control_block()
        .expect("AsyncHandle::get() must be called within a GC thread");

    // 移除條件編譯，始终檢查
    if !tcb.is_scope_active(self.scope_id) {
        panic!(...);
    }
    // ...
}
```

### 修復 to_gc():
添加 scope 有效性檢查：

```rust
pub fn to_gc(self) -> Gc<T> {
    let tcb = crate::heap::current_thread_control_block()
        .expect("AsyncHandle::to_gc() must be called within a GC thread");

    if !tcb.is_scope_active(self.scope_id) {
        panic!(
            "AsyncHandle::to_gc() called after scope was dropped. \
             The AsyncHandleScope that created this handle has been dropped."
        );
    }
    // ...
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此 bug 導致在 scope drop 後仍可訪問 slot 記憶體。當 scope 被 drop 時，其 `HandleBlock` 從 TCB registry 中移除，但記憶體本身未釋放（因為是 `Box<HandleBlock>`）。若新 scope 被創建，相同記憶體可能被重用，導致訪問錯誤的物件。

**Rustacean (Soundness 觀點):**
這是記憶體安全違規。`#[cfg(debug_assertions)]` 條件編譯導致 release 構建中存在 use-after-free 風險。標準庫的設計原則是安全檢查不應因構建類型而異。

**Geohot (Exploit 觀點):**
攻擊者可利用此漏洞在 scope drop 後讀取舊資料。若記憶體被新 scope 重用，可能造成指標混淆，進一步利用實現任意記憶體讀取。
