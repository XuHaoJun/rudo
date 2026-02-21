# [Bug]: GcRwLock::capture_gc_ptrs_into 使用 try_read() 可能導致指標遺漏

**Status:** Fixed
**Tags:** Verified


## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 GC 掃描時剛好有執行緒持有寫鎖 |
| **Severity (嚴重程度)** | Medium | 可能導致 GC 遺漏部分指標，但影響範圍有限 |
| **Reproducibility (復現難度)** | High | 需要精確的執行時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock::capture_gc_ptrs_into`, `sync.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcRwLock::capture_gc_ptrs_into` 應該能夠可靠地捕捉內部資料的所有 GC 指標，即使在並發場景下也應該如此。

### 實際行為 (Actual Behavior)
`GcRwLock::capture_gc_ptrs_into` 使用 `try_read()` 來獲取讀取鎖：

```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Some(value) = self.inner.try_read() {  // 可能失敗！
        value.capture_gc_ptrs_into(ptrs);
    }
}
```

如果此時有執行緒持有寫鎖，`try_read()` 會返回 `None`，導致完全無法捕捉指標。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/sync.rs:600-604`

```rust
impl<T: GcCapture + ?Sized> GcCapture for GcRwLock<T> {
    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Some(value) = self.inner.try_read() {  // Line 601
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

**與 Trace 實作的不一致：**

`GcRwLock` 的 `Trace` 實作 (lines 579-590) 使用 `data_ptr()` 繞過鎖：
```rust
unsafe impl<T: Trace + ?Sized> Trace for GcRwLock<T> {
    fn trace(&self, visitor: &mut impl crate::Visitor) {
        // During STW pause, all mutators are suspended
        let raw_ptr = self.inner.data_ptr();
        unsafe { (*raw_ptr).trace(visitor) }
    }
}
```

這表明在 STW 期間，鎖不會被持有。但 `GcCapture` 使用 `try_read()` 可能在以下場景失敗：
1. 並發標記期間，執行緒仍在運行
2. 使用 lazy sweep 時，掃描線程可能與mutator並發

**為何這可能是 bug：**
- 與 `Trace` 實作的模式不一致
- 在高並發場景下可能遺漏 GC 指標

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcRwLock, Trace, collect_full};
use std::thread;
use std::sync::Arc;
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
    gc_ptr: Option<Gc<Data>>,
}

fn main() {
    let data: Gc<GcRwLock<Data>> = Gc::new(GcRwLock::new(Data {
        value: 0,
        gc_ptr: None,
    }));
    
    let data_clone = data.clone();
    
    // 執行緒持續持有寫鎖
    let writer = thread::spawn(move || {
        loop {
            let mut guard = data_clone.write();
            guard.value += 1;
            guard.gc_ptr = Some(Gc::new(Data {
                value: guard.value,
                gc_ptr: None,
            }));
            drop(guard);
            thread::sleep(Duration::from_millis(1));
        }
    });
    
    // 嘗試觸發 GC
    for _ in 0..100 {
        collect_full();
        thread::sleep(Duration::from_millis(10));
    }
    
    writer.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：使用更強的鎖獲取機制
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    // 使用 read() 而非 try_read()，如果需要等待則阻塞
    let guard = self.inner.read();
    guard.capture_gc_ptrs_into(ptrs);
}
```

選項 2：參考 Trace 實作使用 data_ptr
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    // 類似 Trace 實作，繞過鎖
    // 安全性由 STW 保证
    let raw_ptr = self.inner.data_ptr();
    unsafe {
        (*raw_ptr).capture_gc_ptrs_into(ptrs);
    }
}
```

選項 3：記錄失敗而非靜默忽略
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Some(value) = self.inner.try_read() {
        value.capture_gc_ptrs_into(ptrs);
    } else {
        // 記錄警告或錯誤
        tracing::warn!("Failed to capture GC pointers from GcRwLock - writer held lock");
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在並發 GC 系統中，確保所有 GC 指標都能被掃描是基本要求。使用 `try_read()` 可能在高並發場景下遺漏指標，這與 "no GC pointer left behind" 的原則衝突。

**Rustacean (Soundness 觀點):**
這主要是 API 設計問題。靜默失敗（返回 None）可能導致難以調試的記憶體問題。建議使用明確的錯誤處理或使用阻塞讀取。

**Geohot (Exploit 觀點):**
雖然利用難度較高，但如果攻擊者能夠控制時序，可能：
1. 阻止 GC 正確掃描物件
2. 導致記憶體洩漏（物件被錯誤保留）
3. 在極端情況下可能導致不一致的 GC 狀態

---

**Resolution:** Replaced `try_read()` with blocking `read()` in `GcRwLock::capture_gc_ptrs_into()`. Now always captures inner GC pointers even when a writer holds the lock, ensuring SATB invariance.
