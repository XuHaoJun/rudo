# [Bug]: Handle slot 缺少 null 檢查導致潛在 UAF

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Low` | 需要精確時序：slot 有指標但物件已回收並被重用 |
| **Severity (嚴重程度)** | `Critical` | 可能導致 use-after-free 或讀取無效資料 |
| **Reproducibility (復現難度)** | `Medium` | 需要特定時序觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::get()`, `AsyncHandle::to_gc()`, `AsyncGcHandle::downcast_ref()`, `Handle::get()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

Handle 的 `get()`, `to_gc()`, 和 `downcast_ref()` 方法在解引用 slot 指針之前沒有檢查其是否為 null。

相比之下，同一檔案中的 `iterate()` 方法有正確檢查 null：

```rust
// handles/async.rs:405-409 - 正確的 null 檢查
for slot in slots.iter().take(used) {
    if !slot.is_null() {
        visitor(slot.as_ptr());
    }
}
```

但以下方法缺少此檢查：

1. `AsyncHandle::get()` - `handles/async.rs:582-593`
2. `AsyncHandle::to_gc()` - `handles/async.rs:680-692`  
3. `AsyncGcHandle::downcast_ref()` - `handles/async.rs:1257-1268`
4. `Handle::get()` - `handles/mod.rs:301-314`

### 預期行為
在解引用 slot 指針前應檢查其是否為 null。

### 實際行為
直接解引用 slot 指針，可能導致 UAF。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `AsyncHandle::get()` 中 (`handles/async.rs:582-585`)：

```rust
let slot = unsafe { &*self.slot };
let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
unsafe {
    let gc_box = &*gc_box_ptr;  // <-- 沒有檢查 slot 是否為 null！
    // ...
}
```

對比 `iterate()` 方法的正確模式：

```rust
for slot in slots.iter().take(used) {
    if !slot.is_null() {  // <-- 正確的 null 檢查
        visitor(slot.as_ptr());
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::AsyncHandleScope;

#[derive(Trace)]
struct Data { value: i32 }

async fn bug_poc() {
    let tcb = rudo_gc::heap::current_thread_control_block().unwrap();
    let scope = AsyncHandleScope::new(&tcb);
    
    let gc = Gc::new(Data { value: 42 });
    let handle = scope.handle(&gc);
    
    // 讓 scope 保持活躍，但物件沒有其他 root
    drop(gc);
    
    // 強制 GC 回收物件（物件會被 sweep）
    rudo_gc::collect_full();
    
    // 嘗試使用 handle - slot 指針可能指向已釋放/重用的記憶體
    // 預期：應該檢查 slot 是否為 null 或有效
    // 實際：直接解引用，可能 UAF
    println!("{}", handle.get().value);
}
```

**注意：** 此 PoC 需要精確控制 GC 時序才能穩定重現。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在所有受影響的方法中添加 slot null 檢查：

```rust
pub fn get(&self) -> &T {
    // ... scope 檢查 ...
    
    let slot = unsafe { &*self.slot };
    
    // 添加 null 檢查
    if slot.is_null() {
        panic!("AsyncHandle::get: slot is null - object was collected");
    }
    
    let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
    unsafe {
        let gc_box = &*gc_box_ptr;
        assert!(
            !gc_box.has_dead_flag()
                && gc_box.dropping_state() == 0
                && !gc_box.is_under_construction(),
            "AsyncHandle::get: cannot access a dead, dropping, or under construction Gc"
        );
        gc_box.value()
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 Scheme 的 GC 實現中，handle slot 在物件回收後應該被清除或標記為無效。雖然 `iterate()` 方法正確處理了這種情況，但直接訪問的 API 應該有一致的防護。

**Rustacean (Soundness 觀點):**
這是潛在的記憶體安全問題。直接解引用可能為 null 的指標是未定義行為。即使現有檢查（dead_flag, dropping_state）可能捕獲大多數情況，但在極端時序下可能失效。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過控制 GC 時序來利用此漏洞。當 slot 指標指向已釋放但尚未重用的記憶體時，可能實現任意記憶體讀取。

---

## 關聯 Issue

- bug102: AsyncHandle::get() missing dead/dropping/construction checks (已修復 dead_flag 等檢查，但未修復 null 檢查)
- bug81: AsyncHandle::to_gc UAF (已修復 ref count 增量)
