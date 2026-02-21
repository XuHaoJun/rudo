# [Bug]: Weak::try_upgrade() 缺少 dropping_state 檢查導致 Use-After-Free 風險

**Status:** Open
**Tags:** Not Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 當 Weak reference upgrade 時，物件正在被 drop（ref_count > 0 且 dropping_state != 0） |
| **Severity (嚴重程度)** | Critical | 允許在 dropping_state != 0 時升級會導致 Use-After-Free，違反記憶體安全 |
| **Reproducibility (復現難度)** | Medium | 需要精確的執行時序，但可穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::try_upgrade`, `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Weak::try_upgrade()` 應該與 `Weak::upgrade()` 具有相同的安全檢查，包括檢查 `dropping_state()` 是否不為 0，防止在物件正在被 drop 時建立新的強引用。

### 實際行為 (Actual Behavior)
`Weak::try_upgrade()` 缺少 `dropping_state()` 檢查，而 `Weak::upgrade()` 正確地檢查了此條件。這導致使用 `try_upgrade()` 時可能在物件正在被 drop 的過程中建立新的強引用，導致 Use-After-Free。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/ptr.rs:1555-1610`

`Weak::upgrade()` (lines 1500-1507) 正確檢查了 `dropping_state()`:
```rust
loop {
    if gc_box.has_dead_flag() {
        return None;
    }

    if gc_box.dropping_state() != 0 {  // ✓ 正確檢查
        return None;
    }
    // ...
}
```

但 `Weak::try_upgrade()` (lines 1582-1588) 缺少此檢查:
```rust
loop {
    if gc_box.has_dead_flag() {
        return None;
    }

    // ✗ 缺少 dropping_state() 檢查!

    let current_count = gc_box.ref_count.load(Ordering::Relaxed);
    if current_count == 0 || current_count == usize::MAX {
        return None;
    }
    // ...
}
```

**影響範圍：**
- 當使用 `try_upgrade()` 升級 weak reference 時
- 如果物件正在被 drop（`dropping_state != 0`）但 `ref_count > 0`
- 可能會建立新的強引用，導致 Use-After-Free

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, collect_full};
use std::thread;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // Create a Gc and get a weak reference  
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    // Create another strong reference
    let gc2 = gc.clone();
    
    // Drop one reference, then try to upgrade while other is still alive
    drop(gc);
    
    // At this point, gc2 still holds a reference, but gc is being dropped
    // try_upgrade might succeed even though dropping_state != 0
    if let Some(upgraded) = weak.try_upgrade() {
        // This could be use-after-free if the object is being dropped
        println!("Upgraded: {}", upgraded.value);
    }
    
    drop(gc2);
    collect_full();
}
```
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `crates/rudo-gc/src/ptr.rs` 的 `Weak::try_upgrade()` 方法中添加 `dropping_state()` 檢查：

```rust
pub fn try_upgrade(&self) -> Option<Gc<T>> {
    let ptr = self.ptr.load(Ordering::Acquire);

    let ptr = ptr.as_option()?;

    let addr = ptr.as_ptr() as usize;

    let alignment = std::mem::align_of::<GcBox<T>>();
    if addr % alignment != 0 {
        return None;
    }

    if addr < MIN_VALID_HEAP_ADDRESS {
        return None;
    }
    if !is_gc_box_pointer_valid(addr) {
        return None;
    }

    unsafe {
        // SAFETY: Pointer passed validation checks above (alignment, addr >= 4096)
        let gc_box = &*ptr.as_ptr();

        if gc_box.is_under_construction() {
            return None;
        }

        loop {
            if gc_box.has_dead_flag() {
                return None;
            }

            // 添加這個檢查！
            if gc_box.dropping_state() != 0 {
                return None;
            }

            let current_count = gc_box.ref_count.load(Ordering::Relaxed);
            if current_count == 0 || current_count == usize::MAX {
                return None;
            }
            // ...
        }
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 reference counting GC 中，當物件正在被 drop 時（`dropping_state != 0`），即使 `ref_count > 0`，也不應該允許建立新的強引用。這是因為現有的強引用將會完成 drop 流程，屆時物件會被釋放。新建立的強引用會指向已釋放的記憶體，違反 GC 的記憶體安全 invariant。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題（Memory Safety），不是傳統的 soundness 問題。允許在 `dropping_state != 0` 時建立新的 `Gc<T>` 會導致 Use-After-Free，Rust 的記憶體安全保證被破壞。`try_upgrade()` 應該與 `upgrade()` 具有相同的安全檢查。

**Geohot (Exploit 觀點):**
利用此 bug 需要控制時序：
1. 建立一個 Gc 物件並取得 Weak reference
2. 在另一執行緒中開始 drop 流程（設置 dropping_state）
3. 在 dropping_state 設置後、ref_count 歸零前，呼叫 try_upgrade()
4. 成功建立新的 Gc<T>，指向即將被釋放的記憶體
5. 存取此 Gc<T> 會導致 Use-After-Free
