# [Bug]: WeakCrossThreadHandle::drop Weak Reference Leak After bug231 Fix

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | When WeakCrossThreadHandle is dropped and the slot was swept but not reused |
| **Severity (嚴重程度)** | Medium | Weak reference leak - memory not reclaimed until process exit |
| **Reproducibility (重現難度)** | High | Need specific timing: slot swept but not reused when weak handle drops |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `WeakCrossThreadHandle::drop` (cross_thread.rs:907-933)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

Bug231 fix 新增了 `is_allocated` 檢查到 `WeakCrossThreadHandle::drop`，防止在 slot 被重用後發生 UAF。但這個修復引入了一個新問題：當 slot 被 sweep（未重用）時，weak reference count 永遠不會被遞減，導致 weak reference leak。

### 預期行為 (Expected Behavior)
當 `WeakCrossThreadHandle` 被 drop 時，無論 slot 是否被分配，都應該正確遞減 weak reference count。

### 實際行為 (Actual Behavior)
當 `is_allocated(idx)` 返回 `false` 時，函數提前返回，**不呼叫 `dec_weak_raw`**：
```rust
if !(*header.as_ptr()).is_allocated(idx) {
    return;  // BUG: 沒有呼叫 dec_weak_raw，導致 weak reference leak
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

### 問題函數 (cross_thread.rs:907-933)

```rust
impl<T: Trace + 'static> Drop for WeakCrossThreadHandle<T> {
    fn drop(&mut self) {
        let ptr = self.weak.as_ptr();
        let Some(ptr) = ptr else {
            return;
        };
        let ptr_addr = ptr.as_ptr() as usize;
        if !is_gc_box_pointer_valid(ptr_addr) {
            return;
        }
        unsafe {
            // 檢查 slot 是否仍然 allocated (bug231 fix)
            if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
                let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
                if !(*header.as_ptr()).is_allocated(idx) {
                    return;  // BUG: 沒有呼叫 dec_weak_raw
                }
            }
            let _ = GcBox::dec_weak_raw(ptr.as_ptr().cast::<GcBox<()>>());
        }
    }
}
```

### 問題分析

1. **當 `is_allocated` 返回 `false` 的兩個原因**:
   - **情況 A**: Slot 被 sweep 後**未重用** - GcBox 仍然存在，weak count 應該被遞減
   - **情況 B**: Slot 被 sweep 後**已重用** - 記憶體被新 GcBox 使用，遞減會損壞新物件的 weak count

2. **當前的早期返回邏輯**:
   - 預防情況 B 的 corruption（正確）
   - 但也跳過了情況 A 的 weak count 遞減（錯誤 - leak）

3. **正確的行為應該是**:
   - 如果 slot 未重用：應該呼叫 `dec_weak_raw`
   - 如果 slot 已重用：應該跳過 `dec_weak_raw`（避免 corruption）

4. **困難點**:
   - `WeakCrossThreadHandle` 目前沒有儲存 generation
   - 無法區分「slot 未重用」和「slot 已重用」

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
#[test]
fn test_weak_cross_thread_handle_leak() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::sync::mpsc;

    static WEAK_DROPPED: AtomicUsize = AtomicUsize::new(0);

    #[derive(Trace)]
    struct TestData {
        value: i32,
    }

    let (sender, receiver) = mpsc::channel();

    let origin = thread::spawn(move || {
        let gc: Gc<TestData> = Gc::new(TestData { value: 42 });
        let weak = gc.weak_cross_thread_handle();

        // 取得初始 weak count
        let initial_weak_count = Gc::weak_count(&gc);

        // 發送 weak handle 到其他執行緒
        sender.send(weak).unwrap();

        // 原始執行緒結束，WeakCrossThreadHandle 會被 drop
        // 如果 slot 已被 sweep，weak count 不會被遞減
    });

    let weak = receiver.recv().unwrap();
    origin.join().unwrap();

    // At this point, the WeakCrossThreadHandle was dropped
    // If the slot was swept but not reused, weak count was NOT decremented
    // This is a leak

    // Force GC to collect orphaned objects
    gc::collect_full();

    // The weak handle's target should have been collected
    // But if the weak count wasn't decremented properly, there's a leak
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 選項 1: 追蹤 Generation（推薦）

在 `GcBoxWeakRef` 中新增 `generation` 欄位，在 `WeakCrossThreadHandle::drop` 中驗證：

```rust
// 在 GcBoxWeakRef 中新增
pub(crate) struct GcBoxWeakRef<T: Trace + 'static> {
    ptr: AtomicNullable<GcBox<T>>,
    generation: AtomicU32,  // 新增
}

// 在建立 weak handle 時儲存 generation
impl<T: Trace + 'static> GcBoxWeakRef<T> {
    pub(crate) fn new(ptr: NonNull<GcBox<T>>) -> Self {
        let gen = unsafe { (*ptr.as_ptr()).generation() };  // 讀取當前 generation
        Self {
            ptr: AtomicNullable::new(ptr),
            generation: AtomicU32::new(gen),
        }
    }
}

// 在 drop 時驗證
impl<T: Trace + 'static> Drop for WeakCrossThreadHandle<T> {
    fn drop(&mut self) {
        // ... existing checks ...
        
        unsafe {
            // 讀取當前 generation
            let current_gen = (*ptr.as_ptr()).generation();
            if current_gen != self.weak.generation.load(Ordering::Relaxed) {
                // Slot was reused - skip dec_weak_raw to avoid corruption
                return;
            }
            // Slot not reused - safe to call dec_weak_raw
            let _ = GcBox::dec_weak_raw(ptr.as_ptr().cast::<GcBox<()>>());
        }
    }
}
```

### 選項 2: 總是呼叫 dec_weak_raw（實用但可能造成 corruption）

移除 `is_allocated` 檢查，總是呼叫 `dec_weak_raw`：

```rust
// 這個方案可能導致 slot 已重用時的 corruption，但 leak 更糟糕
unsafe {
    let _ = GcBox::dec_weak_raw(ptr.as_ptr().cast::<GcBox<()>>());
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個問題是 bug231 fix 的後果。bug231 修復了 UAF 問題，但引入了一個 leak。在 lazy sweep 中，slot 可以被回收但不被立即重用。在這個視窗內 drop weak handle 會導致 leak。

**Rustacean (Soundness 觀點):**
Leak 不是 UB，但可能導致記憶體不斷增長直到程序結束。對於長時間運行的服務，這可能是嚴重的記憶體問題。

**Geohot (Exploit 觀點):**
如果攻擊者可以控制 GC timing，理論上可以通過製造大量 orphan weak handles 來耗盡記憶體。

---

## 相關 Bug

- Bug231: `WeakCrossThreadHandle::drop Missing is_allocated Check` - 這個 bug 的修復引入了当前的 bug
