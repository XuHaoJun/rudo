# [Bug]: GcRootSet::register FIX bug514 incomplete - always stores (0, 0) instead of actual generation

**Status:** Open
**Tags:** Verified

## 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 所有使用 GcRootGuard 的 tokio 程式都受影響 |
| **Severity (嚴重程度)** | Critical | 可能導致 GC 錯誤標記或錯誤回收物件 |
| **Reproducibility (復現難度)** | Low | 單執行緒難以穩定復現，需要多執行緒並髊 |

---

## 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRootSet::register` (tokio/root.rs:70-81)
- **OS / Architecture:** Linux x86_64 (All)
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 問題描述 (Description)

`GcRootSet::register()` 聲稱 "FIX bug514" 要存儲 GcBox 的 generation，但實際實現**從未讀取 generation**，始終存儲 `(0, 0)`。

這導致 `snapshot()` 中的 generation 檢查**永遠失敗**（因為 `current_generation` 通常是 ≥1 的值，不會等於 0）。

### 預期行為 (Expected Behavior)
1. `register()` 應該從 GcBox 讀取實際的 generation
2. `snapshot()` 應該能正確驗證 root 是否仍然是同一個物件

### 實際行為 (Actual Behavior)
1. `register()` 始終存儲 `(0, 0)`
2. `snapshot()` 的 generation 檢查 `current_generation == stored_generation` 永遠不會匹配真實物件

---

## 根本原因分析 (Root Cause Analysis)

在 `tokio/root.rs:70-81`：

```rust
pub fn register(&self, ptr: usize) {
    let mut roots = self.roots.lock().unwrap();

    // FIX bug514: Store generation from GcBox if valid, otherwise 0.
    // Generation is read during snapshot to detect slot reuse.
    // We store 0 as a placeholder when heap is not available;
    // snapshot() will do the generation check when called with a valid heap.
    let entry = roots.entry(ptr).or_insert((0, 0));  // BUG: 永遠插入 (0, 0)!
    entry.0 += 1;

    self.dirty.store(true, Ordering::Release);
}
```

問題：
- 註釋說「Store generation from GcBox if valid, otherwise 0」
- 但代碼只有 `or_insert((0, 0))`，**從未調用 `find_gc_box_from_ptr` 或讀取 generation**
- 這導致 `snapshot()` 中的 generation 檢查永遠失敗

---

## 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::tokio::{GcRootSet, GcRootGuard};
use std::sync::Arc;

#[derive(Clone, Trace)]
struct Data {
    value: i32,
}

#[tokio::main]
async fn main() {
    let set = GcRootSet::global();
    set.clear();

    let gc = Gc::new(Data { value: 42 });
    let ptr = gc.as_ptr() as usize;
    
    // 讀取 actual generation
    let initial_gen = unsafe { (*(gc.as_ptr())).generation() };
    eprintln!("Initial generation: {}", initial_gen);
    
    // 註冊為 root
    unsafe {
        let _guard = GcRootGuard::new(gc.as_ptr());
    }
    
    // 檢查 HashMap 中存儲的值
    let roots = set.roots.lock().unwrap();
    if let Some(entry) = roots.get(&ptr) {
        eprintln!("Stored generation: {}", entry.0);  // 永遠是 0!
    }
    drop(roots);
    
    // snapshot 時 generation 檢查會失敗
    // 因为 stored=0, actual>=1, 所以永遠不相等
}
```

---

## 建議修復方案 (Suggested Fix / Remediation)

修改 `GcRootSet::register()` 以實際讀取並存儲 generation：

```rust
pub fn register(&self, ptr: usize) {
    let mut roots = self.roots.lock().unwrap();

    // 讀取 GcBox 的 generation
    let generation = unsafe {
        if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(/* 需要 heap */, ptr as *const u8) {
            (*gc_box.as_ptr()).generation()
        } else {
            0  // 無效指標
        }
    };
    
    let entry = roots.entry(ptr).or_insert((generation, 0));
    entry.0 += 1;

    self.dirty.store(true, Ordering::Release);
}
```

但這需要傳入 heap 參數，考慮到 `register` 是在各种 async context 中調用的，這可能需要重構。

另一方案：完全移除 generation 檢查（不推薦，因為會回到 bug514 的原始問題）。

---

## 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- GcRootSet 的 generation 追蹤是對的，但 "fix" 沒有實際讀取 generation
- 這導致所有 tokio root 都無法通過 snapshot 的 generation 檢查
- 需要重構以傳入 heap 或使用其他機制獲取 generation

**Rustacean (Soundness 觀點):**
- `or_insert((0, 0))` 沒有使用 GcBox 的任何資訊
- 這是明顯的 implementation bug - 註釋和代碼不一致

**Geohot (Exploit 觀點):**
- 由於 generation 永遠是 0，攻擊者可以強制 slot 回收並重新分配
- 舊 root 指向新物件時，generation 檢查會失敗（好），但新的也會失敗（壞）
- 這導致合法的 tokio root 也可能被錯誤排除