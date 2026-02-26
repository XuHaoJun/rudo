# [Bug]: Weak::clone() 缺少 dead_flag / dropping_state 檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要在物件已死亡或正在 dropping 時 clone Weak |
| **Severity (嚴重程度)** | Medium | 可能導致為已死亡物件增加 weak count，導致記憶體管理不一致 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak<T>::clone()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當呼叫 `Weak::clone()` 時，如果物件已經死亡（`has_dead_flag()` 為 true）或正在被 drop（`dropping_state() != 0`），應該返回 null Weak 或執行失敗。

這與以下方法的行為一致：
- `Weak::upgrade()` - 有檢查 has_dead_flag() 和 dropping_state()
- `Gc::clone()` - 有檢查 has_dead_flag() 和 dropping_state()
- `Gc::downgrade()` - 有檢查 has_dead_flag() 和 dropping_state()

### 實際行為 (Actual Behavior)

目前 `Weak::clone()` **沒有**檢查：
- `has_dead_flag()`
- `dropping_state()`

直接調用 `inc_weak()` 而不檢查物件狀態，導致可能為已死亡或正在 dropping 的物件增加 weak count。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `ptr.rs:1817-1844` (`Weak<T>::clone()`)

對比 `Weak::upgrade()` (ptr.rs:1550-1589) 有正確的檢查：

```rust
pub fn upgrade(&self) -> Option<Gc<T>> {
    // ...
    unsafe {
        let gc_box = &*ptr.as_ptr();

        loop {
            if gc_box.has_dead_flag() {  // 有檢查！
                return None;
            }

            if gc_box.dropping_state() != 0 {  // 有檢查！
                return None;
            }
            // ...
        }
    }
}
```

但 `Weak::clone()` 缺少這些檢查：

```rust
impl<T: Trace> Clone for Weak<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire);
        // ... pointer validation ...
        
        // 缺少: has_dead_flag() 和 dropping_state() 檢查！
        
        unsafe {
            (*ptr.as_ptr()).inc_weak();  // 直接增加計數
        }
        // ...
    }
}
```

這與 bug63 發現的 `cross_thread_handle()` / `weak_cross_thread_handle()` 缺少檢查的問題類似。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, collect_full};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 1. 創建一個 Gc 並取得 Weak
    let gc = Gc::new(Data { value: 42 });
    let weak = Gc::downgrade(&gc);
    
    // 2. 強制觸發 GC 來 drop 這個對象
    // (需要通過特定方式讓對象被標記為 dead)
    collect_full();
    
    // 3. 此時 gc 應該被視為 "dead"，但 Weak 本身仍然有效
    // (ptr not null)
    
    // 4. 調用 Weak::clone - 應該返回 null Weak 或失敗
    // 但實際上會成功創建新的 Weak 並增加 weak_count
    let weak2 = weak.clone();
    
    // 類似於 cross_thread_handle 的問題（bug63）
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Weak::clone()` 中添加檢查：

```rust
impl<T: Trace> Clone for Weak<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire);
        let Some(ptr) = ptr.as_option() else {
            return Self {
                ptr: AtomicNullable::null(),
            };
        };
        let ptr_addr = ptr.as_ptr() as usize;
        let alignment = std::mem::align_of::<GcBox<T>>();
        if ptr_addr % alignment != 0 || ptr_addr < MIN_VALID_HEAP_ADDRESS {
            return Self {
                ptr: AtomicNullable::null(),
            };
        }
        // Validate pointer is still in heap before dereferencing (avoids TOCTOU with sweep).
        if !is_gc_box_pointer_valid(ptr_addr) {
            return Self {
                ptr: AtomicNullable::null(),
            };
        }
        
        // 新增: 檢查 dead_flag 和 dropping_state
        unsafe {
            let gc_box = &*ptr.as_ptr();
            if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 {
                return Self {
                    ptr: AtomicNullable::null(),
                };
            }
            gc_box.inc_weak();
        }
        
        Self {
            ptr: AtomicNullable::new(ptr),
        }
    }
}
```

這與 `Weak::upgrade()` 的行為一致，確保在物件已死亡或正在 dropping 時，clone 會返回 null Weak。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
當物件被標記為 dead 或正在 dropping 時，不應該允許建立新的 weak 引用。這與 reference counting 的基本原則不符：為一個已經無效的物件增加引用計數會導致不正確的記憶體管理。

**Rustacean (Soundness 觀點):**
這是一個記憶體管理一致性問題。允許為已死亡或正在 drop 的物件建立 weak 引用可能導致：
1. 為無效物件增加 weak count
2. 記憶體管理不一致
3. 潛在的 double-free 或 leak

**Geohot (Exploit 攻擊觀點):**
此漏洞可以被利用來：
1. 繞過 GC 的安全檢查
2. 創建對已釋放物件的 weak 引用
3. 導致記憶體管理不一致

---

## ✅ 驗證記錄 (Verification Record)

**Date:** 2026-02-22
**Verified by:** Code analysis

**Verification Details:**
Confirmed bug exists in current codebase at `ptr.rs:1817-1844`:

```rust
impl<T: Trace> Clone for Weak<T> {
    fn clone(&self) -> Self {
        let ptr = self.ptr.load(Ordering::Acquire);
        // ... pointer validation ...
        
        // 缺少: has_dead_flag() 和 dropping_state() 檢查！
        
        unsafe {
            (*ptr.as_ptr()).inc_weak();  // 直接增加計數，沒有驗證物件狀態
        }
        // ...
    }
}
```

對比 `Weak::upgrade()` (ptr.rs:1550-1600) 有正確的檢查：
- Line 1564: `if gc_box.has_dead_flag()` 
- Line 1568: `if gc_box.dropping_state() != 0`

**Status:** Bug confirmed. Issue remains Open, marked as Verified.

---

## Resolution (2026-02-26)

**Outcome:** Already fixed.

The fix was applied in commit `b9db90f` ("fix: add safety checks to Weak::clone and GcRwLockWriteGuard/GcMutexGuard Drop"). The current `Weak::clone()` implementation in `ptr.rs` (lines 1885–1894) correctly checks:
- `gc_box.has_dead_flag()` → returns null Weak
- `gc_box.dropping_state() != 0` → returns null Weak

Behavior now matches `Weak::upgrade()` as described in the issue.

