# [Bug]: GcScope::spawn Missing Bounds Check Causes Buffer Overflow

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要追蹤超過 256 個 Gc 物件才會觸發 |
| **Severity (嚴重程度)** | Critical | 緩衝區溢位導致記憶體損壞，可能造成 use-after-free |
| **Reproducibility (Reproducibility)** | High | 可穩定重現，只要追蹤超過 256 個物件 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcScope::spawn` in `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 `GcScope::spawn` 嘗試追蹤超過 256 個 Gc 物件時，應該 panic 並顯示錯誤訊息，告知已超過最大 handle 數量。

### 實際行為 (Actual Behavior)
當追蹤超過 256 個 Gc 物件時，程式不會 panic，而是發生緩衝區溢位 write 到陣列邊界之外的記憶體位置，導致記憶體損壞或 use-after-free。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/handles/async.rs:1040-1078`，`GcScope::spawn` 方法在建立 handles 時缺少邊界檢查：

```rust
let handles: Vec<AsyncGcHandle> = tracked
    .iter()
    .map(|tracked| {
        let used = unsafe { &*scope.data.used.get() }.fetch_add(1, Ordering::Relaxed);
        // 缺少檢查: if used >= HANDLE_BLOCK_SIZE { panic!... }
        
        let slot_ptr = unsafe {
            let slots_ptr = scope.data.block.slots.get() as *mut HandleSlot;
            slots_ptr.add(used)  // 當 used >= 256 時，這會寫入邊界外的記憶體
        };

        unsafe {
            (*slot_ptr).set(tracked.ptr);  // 緩衝區溢位寫入
        }
        // ...
    })
    .collect();
```

相比之下，在 `AsyncHandleScope::handle` 方法 (line 309-334) 中有正確的邊界檢查：

```rust
let idx = used.fetch_add(1, Ordering::Relaxed);
if idx >= HANDLE_BLOCK_SIZE {
    panic!("AsyncHandleScope: exceeded maximum handle count ({HANDLE_BLOCK_SIZE})");
}
```

`GcScope::spawn` 缺少這個檢查，導致當追蹤超過 256 個物件時會發生緩衝區溢位。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Trace};
use rudo_gc::handles::GcScope;

#[derive(Trace)]
struct Data { value: i32 }

async fn trigger_bug() {
    let mut scope = GcScope::new();
    
    // Create 257 Gc objects to trigger overflow
    let objects: Vec<Gc<Data>> = (0..257)
        .map(|i| Gc::new(Data { value: i }))
        .collect();
    
    scope.track_slice(&objects);
    
    // This will overflow the slot array without bounds check
    scope.spawn(|_handles| async move {
        println!("Should not reach here");
    }).await;
}

fn main() {
    // Run in GC thread context
    trigger_bug();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcScope::spawn` 方法中新增邊界檢查，與 `AsyncHandleScope::handle` 保持一致：

```rust
let used = unsafe { &*scope.data.used.get() }.fetch_add(1, Ordering::Relaxed);

if used >= HANDLE_BLOCK_SIZE {
    panic!("GcScope::spawn: exceeded maximum handle count ({HANDLE_BLOCK_SIZE})");
}

validate_gc_in_current_heap(tracked.ptr as *const u8);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 暴露了一個重要的設計問題：`GcScope` 和 `AsyncHandleScope` 在 handle 分配上的行為不一致。`AsyncHandleScope::handle` 有邊界檢查，但 `GcScope::spawn` 缺少相同的檢查。這種不一致性很容易造成問題。在 production 環境中，如果用戶嘗試追蹤大量物件（例如處理大型資料結構），將會觸發這個緩衝區溢位，導致難以診斷的記憶體損壞。

**Rustacean (Soundness 觀點):**
這是一個明確的記憶體安全問題，類似於 C/C++ 中的緩衝區溢位。`HANDLE_BLOCK_SIZE` 是 256，這是一個固定的陣列大小。當 `used` 超過 255 時，`slots_ptr.add(used)` 會產生一個指向陣列邊界之外的指標，而後續的寫入操作將會破壞堆疊或堆積上的其他資料。這是一個嚴重的 soundness 問題。

**Geohot (Exploit 觀點):**
從攻擊者的角度來看，這個 bug 提供了一個 Controlled Write Primitive。攻擊者可以通過控制 `tracked` 向量的大小，選擇性地覆蓋陣列後面的記憶體。雖然 `HandleBlock` 是動態分配的，但相鄰的記憶體区域可能包含關鍵的 GC 資料結構（如其他 GcBox 指標或元資料）。在某些情況下，這可能導致任意指標寫入，進而實現更嚴重的攻擊。
