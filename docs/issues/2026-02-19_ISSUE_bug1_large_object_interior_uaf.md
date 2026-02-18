# [Bug]: 大型物件內部指標在執行緒終止後失效導致 UAF

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要跨執行緒使用內部指標，但此場景較少見 |
| **Severity (嚴重程度)** | Critical | 執行緒終止後存取記憶體導致 use-after-free |
| **Reproducibility (復現難度)** | Medium | 需要特定的使用模式（執行緒終止 + 內部指標） |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GlobalSegmentManager::large_object_map`, `LocalHeap::drop`, `find_gc_box_from_ptr`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

當分配大型物件的執行緒終止後，該執行緒的 heap 會被孤立（orphaned）。此時，`LocalHeap::drop` 會從 `GlobalSegmentManager::large_object_map` 中移除該執行緒的所有大型物件條目。這導致 `find_gc_box_from_ptr` 函數無法通過內部指標（interior pointer）找到正確的 GC 框塊，進而在堆疊掃描時遺漏這些物件。

### 預期行為
當大型物件的執行緒終止後，物件應該保持有效（因為可能仍有其他執行緒持有引用）。透過內部指標應該仍能正確解析到 GC 框塊。

### 實際行為
1. 執行緒 A 分配大型物件，獲取其內部指標
2. 執行緒 A 終止，`LocalHeap::drop` 觸發
3. `large_object_map` 中的條目被移除
4. 另一個執行緒進行 GC 時，透過內部指標進行保守式掃描
5. 由於無法找到對應的 GC 框塊，物件被錯誤地視為垃圾回收

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `heap.rs` 的 `LocalHeap::drop` 實作中：

```rust
// 當執行緒終止時，large_object_map 的條目被移除
manager.large_object_map.retain(|addr, _| {
    !(*local_heap).is_in_range(*addr)
});
```

這導致：
- 內部指標無法映射回 GC 框塊
- 保守式堆疊掃描可能錯過這些物件
- 仍被引用的物件被錯誤回收

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{collect_full, Gc, Trace};
use std::sync::{atomic::AtomicUsize, Arc};
use std::thread;

struct LargeStruct {
    data: [u64; 10000],
}

unsafe impl Trace for LargeStruct {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

fn main() {
    let interior_ptr_addr = Arc::new(AtomicUsize::new(0));

    let handle = thread::spawn({
        let interior_ptr_addr = interior_ptr_addr.clone();
        move || {
            let gc = Gc::new(LargeStruct { data: [0x42; 10000] });
            let ptr = std::ptr::from_ref(&gc.data[8500]).cast::<u8>();
            interior_ptr_addr.store(ptr as usize, std::sync::atomic::Ordering::SeqCst);
            gc
        }
    });

    let gc = handle.join().unwrap();
    let ptr_addr = interior_ptr_addr.load(std::sync::atomic::Ordering::SeqCst);

    drop(gc);
    collect_full();

    // 嘗試存取記憶體 - 如果 bug 存在，這裡會 UAF
    unsafe {
        let ptr = ptr_addr as *const u8;
        let _value = *ptr.cast::<u64>();
    }
}
```

執行測試：
```bash
cargo test --test bug1_large_object_interior_uaf -- --test-threads=1
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：延遲移除大型物件映射（推薦）
不要在 `LocalHeap::drop` 中立即移除 `large_object_map` 條目。相反，應該依賴 GC 的標記-清除階段來清理孤立的的大型物件。這確保了內部指標在物件被實際回收前仍然有效。

### 方案 2：改進內部指標解析
在 `find_gc_box_from_ptr` 中，對於無法在 `large_object_map` 中找到的指標，應該掃描 orphan pages 來找到對應的 GC 框塊。

### 方案 3：保留映射直到所有引用消失
使用引用計數追蹤有多少執行緒仍然需要訪問該大型物件，只在引用計數歸零時才移除映射。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
此問題反映了 BiBOP 配置與執行緒本地 heap 生命週期管理的複雜性。在傳統 GC 中，物件的生命週期與執行緒無關，但 rudo-gc 的執行緒本地分配要求我們更謹慎地處理執行緒終止時的物件遷移。

**Rustacean (Soundness 觀點):**
這是記憶體安全問題。當物件被錯誤回收後，透過內部指標存取記憶體會導致 use-after-free，這是未定義行為。

**Geohot (Exploit 觀點):**
攻擊者可以通過控制執行緒終止時機來實現：
1. 任意記憶體讀取（透過 UAF）
2. 記憶體佈局洩露（透過觀察 GC 回收行為）
