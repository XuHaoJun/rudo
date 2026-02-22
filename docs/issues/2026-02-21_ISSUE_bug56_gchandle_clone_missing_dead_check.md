# [Bug]: GcHandle::clone() Missing Dead Flag Check 導致潛在記憶體不安全

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需要物件被標記為 dead 但 handle 仍存活的邊界情況 |
| **Severity (嚴重程度)** | `High` | 可能導致 Use-After-Free 或引用計數腐敗 |
| **Reproducibility (復現難度)** | `Medium` | 需要精確控制 GC 時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle<T>` clone implementation
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcHandle::clone()` 應該在遞增引用計數前檢查物件是否已標記為 dead（`has_dead_flag()`）或正在 drop（`dropping_state() != 0`），以確保不會對已棄置的物件進行操作。

### 實際行為 (Actual Behavior)
`GcHandle::clone()` 實作 (cross_thread.rs:224-244) 直接遞增引用計數，沒有檢查 `has_dead_flag()` 或 `dropping_state()`。這與 `Gc::clone()` 的行為類似（bug46），但發生在不同的類型上。

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題點
`crates/rudo-gc/src/handles/cross_thread.rs:224-244`

```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        assert_ne!(
            self.handle_id,
            HandleId::INVALID,
            "cannot clone an unregistered GcHandle"
        );

        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        let new_id = roots.allocate_id();
        roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());
        unsafe { (*self.ptr.as_ptr()).inc_ref() };  // <-- 沒有檢查 has_dead_flag()!
        drop(roots);

        Self {
            ptr: self.ptr,
            origin_tcb: Arc::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
            handle_id: new_id,
        }
    }
}
```

### 對比：Gc::clone() 的正確模式 (ptr.rs)
根據 bug46 的修復，`Gc::clone()` 應該檢查：
```rust
if (*gc_box_ptr).has_dead_flag() {
    panic!("cannot clone dead Gc");
}
```

### 邏輯缺陷
1. `GcHandle::clone()` 直接遞增 ref_count，沒有驗證物件是否為 dead 或 dropping
2. 當物件被標記為 dead 但 handle 仍然存在時，clone 會錯誤地遞增計數
3. 這與 `Gc::clone()`（bug46）和其他安全實作的模式不一致

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace, GcHandle, collect_full};
use std::thread;
use std::sync::Arc;

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 1. Create GcHandle from another thread
    let gc = Gc::new(GcCell::new(Data { value: 42 }));
    let handle = gc.cross_thread_handle();
    
    // 2. Promote to old generation
    collect_full();
    
    // 3. Drop original Gc to mark as dead
    drop(gc);
    
    // 4. Force GC to mark object dead
    collect_full();
    
    // 5. Clone handle - should fail but incorrectly increments ref
    let _handle2 = handle.clone();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcHandle::clone()` 中新增 dead flag 檢查：

```rust
impl<T: Trace + 'static> Clone for GcHandle<T> {
    fn clone(&self) -> Self {
        assert_ne!(
            self.handle_id,
            HandleId::INVALID,
            "cannot clone an unregistered GcHandle"
        );

        // Add dead flag check
        unsafe {
            if (*self.ptr.as_ptr()).has_dead_flag() {
                panic!("cannot clone dead GcHandle");
            }
            if (*self.ptr.as_ptr()).dropping_state() != 0 {
                panic!("cannot clone GcHandle being dropped");
            }
            (*self.ptr.as_ptr()).inc_ref();
        }

        let mut roots = self.origin_tcb.cross_thread_roots.lock().unwrap();
        let new_id = roots.allocate_id();
        roots.strong.insert(new_id, self.ptr.cast::<GcBox<()>>());
        drop(roots);

        Self {
            ptr: self.ptr,
            origin_tcb: Arc::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
            handle_id: new_id,
        }
    }
}
```

或者參考 bug46 的修復模式，使用更優雅的方式處理。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
GcHandle 是跨執行緒的 root 追蹤機制。當物件被標記為 dead 後，任何對其引用計數的操作都可能導致記憶體管理錯誤。這種情況在跨執行緒場景尤其危險，因為物件的生命週期更难預測。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。對 dead 物件遞增引用計數可能導致：
1. 錯誤的 ref_count 值
2. Double-free 或 use-after-free
3. 與 bug46（Gc::clone）類似的模式，應該一併修復

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個漏洞：
1. 透過特定時序使物件被標記為 dead
2. 同時保持 GcHandle 存活
3. 呼叫 clone 導致 ref_count 腐敗
4. 可能造成記憶體損壞或 double-free

---

## 📌 備註 (Notes)

- 與 bug46（Gc::clone missing dead flag）互補
- bug44 和 bug46 分別從不同角度覆蓋了 Gc::clone 的問題
- 此 bug 專門針對 GcHandle<T> 類型

---

## Resolution

`GcHandle::clone()` 已於 handles/cross_thread.rs 加入 `has_dead_flag()` 與 `dropping_state()` 檢查。當物件為 dead 或 dropping 時 panic，與 `Gc::clone()` 行為一致。
