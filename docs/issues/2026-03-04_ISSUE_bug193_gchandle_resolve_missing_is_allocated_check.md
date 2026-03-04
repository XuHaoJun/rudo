# [Bug]: GcHandle::resolve 缺少 is_allocated 檢查 - 可能訪問錯誤物件

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 handle resolve 並發執行 |
| **Severity (嚴重程度)** | High | 可能導致訪問錯誤物件的數據，造成記憶體損壞 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發並發競爭 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::resolve()`, `GcHandle::try_resolve()`, `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcHandle::resolve()` 和 `GcHandle::try_resolve()` 方法缺少 `is_allocated()` 檢查。這可能導致在 lazy sweep 與 handle resolution 並發執行時，訪問到錯誤的物件。

### 預期行為 (Expected Behavior)
在遞增 ref_count 並返回 Gc 之前，應該先檢查該 slot 是否仍然被分配。如果 slot 已被 sweep 且重用，則不應該繼續訪問。

### 實際行為 (Actual Behavior)
`resolve()` 檢查了：
- `is_under_construction()`
- `has_dead_flag()`
- `dropping_state()`

但**缺少** `is_allocated()` 檢查！

對比：`mark_object_black` (incremental.rs:1002) 和 parallel marking (bug78) 都有正確的 `is_allocated` 檢查。

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 lazy sweep 與 handle resolution 並發執行時：
1. Object A 分配在 slot i
2. GcHandle 創建，指向 Object A
3. Object A 變得不可達，進入 mark 階段
4. **關鍵**：在 mark 完成前，lazy sweep 提前運行
5. Lazy sweep 檢查 `!is_marked(i)`，因為 Object A 還沒被標記，所以被 sweep
6. Slot i 被添加到 free list
7. **新 Object B** 分配到 slot i（新的 GcBox）
8. GcHandle 仍然指向原來的記憶體位址
9. 調用 `resolve()` → 返回的是 Object B 的數據！

或者在 incremental marking 場景：
1. Incremental mark 運行中，部分物件已標記
2. Lazy sweep 同時運行，sweep 未標記的物件
3. Slot 被新 allocation 重用
4. Handle resolve 訪問到新物件

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
// 需要精確控制並發時序
use rudo_gc::{Gc, Trace, collect_full};
use std::thread;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    // 1. 創建 Gc 物件和 handle
    let gc = Gc::new(Data { value: 1 });
    let handle = gc.cross_thread_handle();
    
    // 2. 觸發 GC 將物件變得不可達
    drop(gc);
    
    // 3. 同時運行：
    //    - Thread A: 進行 incremental/parallel marking
    //    - Thread B: 進行 lazy sweep，恰好重用同一個 slot
    
    // 4. 調用 resolve() - 可能返回錯誤的物件！
    let resolved = handle.resolve();
    println!("{}", resolved.value); // 可能不是預期的值！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

在 `GcHandle::resolve()` 和 `GcHandle::try_resolve()` 中添加 `is_allocated` 檢查：

```rust
// handles/cross_thread.rs: resolve()
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    
    // Get page header and check is_allocated
    let (header, idx) = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8)
        .and_then(|h| {
            crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8)
                .map(|i| (h, i))
        })
        .expect("Invalid GC pointer");
        
    if !(*header.as_ptr()).is_allocated(idx) {
        panic!("GcHandle::resolve: slot has been swept and reused");
    }
    
    assert!(!gc_box.is_under_construction(), ...);
    assert!(!gc_box.has_dead_flag(), ...);
    assert!(gc_box.dropping_state() == 0, ...);
    
    gc_box.inc_ref();
    Gc::from_raw(self.ptr.as_ptr() as *const u8)
}
```

類似的修復也需要應用於 `try_resolve()`（返回 `None` 而不是 panic）。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在傳統 GC 實現中，handle 應該在 sweep 之前驗證物件的有效性。Lazy sweep 與標記並發時，必須確保 handle 不會訪問到已被回收並重用的 slot。這與 bug78（parallel marking 缺少 is_allocated 檢查）是同樣的模式。

**Rustacean (Soundness 觀點):**
這是一個潛在的 soundness 問題。如果 handle 訪問到錯誤的物件（slot 被重用後的新物件），可能導致類型混淆或數據損壞。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個 race condition 來：
1. 讀取其他物件的數據（資訊洩漏）
2. 造成邏輯錯誤（程式基於錯誤的數據做決策）
3. 破壞記憶體一致性

---

## 🔗 相關 Issue

- bug78: Parallel marking 缺少 is_allocated 檢查（已修復）
- bug39: GcHandle::resolve() 缺少有效性檢查（已修復，但缺少 is_allocated）
- bug135: Lazy sweep gen_old_flag 未清除（已修復）

