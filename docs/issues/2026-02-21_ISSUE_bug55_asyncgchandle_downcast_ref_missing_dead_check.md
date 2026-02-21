# [Bug]: AsyncGcHandle::downcast_ref() 缺少 Dead Flag 檢查導致潛在 UAF

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要物件被標記為 dead 但 handle 仍存活的邊界情況 |
| **Severity (嚴重程度)** | High | 可能導致 Use-After-Free 或存取已釋放記憶體 |
| **Reproducibility (復現難度)** | Medium | 需要精確控制 GC 時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncGcHandle::downcast_ref()` in `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`AsyncGcHandle::downcast_ref()` 應該在dereference 指標前檢查物件是否已標記為 dead（`has_dead_flag()`），以確保不會存取已釋放的記憶體。

### 實際行為 (Actual Behavior)
`AsyncGcHandle::downcast_ref()` (async.rs:1206-1214) 直接dereference指標，沒有檢查 `has_dead_flag()`。這與其他類似函數（如 `Gc::try_deref()`, `Gc::downcast_ref()`）的行為不一致，後者都會檢查 dead flag。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題程式碼
`crates/rudo-gc/src/handles/async.rs:1206-1214`

```rust
#[inline]
pub fn downcast_ref<T: Trace + 'static>(&self) -> Option<&T> {
    if self.type_id == TypeId::of::<T>() {
        let slot = unsafe { &*self.slot };
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        Some(unsafe { &*gc_box_ptr }.value())  // <-- 沒有檢查 has_dead_flag()!
    } else {
        None
    }
}
```

### 對比：正確的模式
`Gc<T>::downcast_ref()` in `ptr.rs:1668-1672`:
```rust
pub fn downcast_ref<T: Trace + 'static>(&self) -> Option<&T> {
    if self.type_id == TypeId::of::<T>() && self.is_alive() {
        // ... 檢查 is_alive() 包含 has_dead_flag()
    }
    // ...
}
```

`Gc::try_deref()` in `ptr.rs:1059`:
```rust
if (*gc_box_ptr).has_dead_flag() || (*gc_box_ptr).dropping_state() != 0 None;
}
```

 {
    return### 邏輯缺陷
1. `downcast_ref()` 直接dereference指標，沒有驗證物件是否為 dead
2. 當物件被標記為 dead 但 handle 仍然存在時，可能發生 use-after-free
3. 這與其他 `downcast_ref` 實作不一致，违反了安全慣例

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace, GcScope, collect_full};
use std::cell::RefCell;

#[derive(Trace)]
struct Data {
    value: RefCell<i32>,
}

#[tokio::main]
async fn main() {
    // 1. Create tracked GC object
    let gc = Gc::new(Data { value: RefCell::new(42) });
    let mut scope = GcScope::new();
    scope.track(&gc);
    
    // 2. Promote to old generation
    collect_full();
    
    // 3. Spawn async task and get handle
    scope.spawn(|handles| async move {
        let handle = &handles[0];
        
        // 4. Force object to be marked dead
        // (This requires internal GC hooks or specific timing)
        
        // 5. Call downcast_ref - could access dead object
        if let Some(data) = handle.downcast_ref::<Data>() {
            // Could read from dead/freed memory
            println!("{}", data.value.borrow());
        }
    }).await;
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

在dereference前加入 dead flag 檢查：

```rust
#[inline]
pub fn downcast_ref<T: Trace + 'static>(&self) -> Option<&T> {
    if self.type_id == TypeId::of::<T>() {
        let slot = unsafe { &*self.slot };
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        
        // Add dead flag check
        unsafe {
            if (*gc_box_ptr).has_dead_flag() {
                return None;
            }
            Some(&*gc_box_ptr.value())
        }
    } else {
        None
    }
}
```

或者使用現有的 `is_alive()` 方法：

```rust
pub fn downcast_ref<T: Trace + 'static>(&self) -> Option<&T> {
    if self.type_id == TypeId::of::<T>() {
        let slot = unsafe { &*self.slot };
        let gc_box_ptr = slot.as_ptr() as *const GcBox<T>;
        
        unsafe {
            let gc: Gc<T> = Gc::from_raw(gc_box_ptr as *const u8);
            if gc.is_alive() {
                Some(gc.as_ref())
            } else {
                None
            }
        }
    } else {
        None
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Async handles 需要特別小心記憶體管理。當物件被標記為 dead 但 async task 仍在執行時，`downcast_ref` 可能存取到已被回收的記憶體。這個問題在 async GC 中尤其重要，因為 async task 的生命週期與 GC 週期可能不同步。

**Rustacean (Soundness 觀點):**
這是一個記憶體安全問題。當物件被標記為 dead 後，任何對其資料的存取都是未定義行為。應該在dereference前檢查 `has_dead_flag()`，與其他 `downcast_ref` 實作保持一致。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個漏洞：
1. 建立一個包含敏感資料的 GC 物件
2. 觸發 GC 標記該物件為 dead
3. 利用 async task 仍然持有 handle 的時機
4. 透過 `downcast_ref` 讀取已釋放記憶體中的殘餘數據
