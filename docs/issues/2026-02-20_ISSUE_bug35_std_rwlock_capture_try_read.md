# [Bug]: std::sync::RwLock 的 GcCapture 實作使用 try_read() 可能導致指標遺漏

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要在 GC 掃描時剛好有執行緒持有寫鎖 |
| **Severity (嚴重程度)** | Medium | 可能導致 GC 遺漏部分指標，但影響範圍有限 |
| **Reproducibility (復現難度)** | High | 需要精確的執行時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcCapture` impl for `std::sync::RwLock<T>`, `cell.rs:567-579`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcCapture` for `std::sync::RwLock<T>` 應該能夠可靠地捕捉內部資料的所有 GC 指標，即使在並發場景下也應該如此。

### 實際行為 (Actual Behavior)
`std::sync::RwLock<T>` 的 `GcCapture` 實作使用 `try_read()` 來獲取讀取鎖：

```rust
// cell.rs:573-578
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Ok(value) = self.try_read() {  // 可能失敗！
        value.capture_gc_ptrs_into(ptrs);
    }
}
```

如果此時有執行緒持有寫鎖，`try_read()` 會返回 `Err`，導致完全無法捕捉指標。

**這與 bug34 描述的 GcRwLock 問題相同，但發生在不同的類型上。**

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題位置：** `crates/rudo-gc/src/cell.rs:573-578`

```rust
impl<T: GcCapture + 'static> GcCapture for std::sync::RwLock<T> {
    #[inline]
    fn capture_gc_ptrs(&self) -> &[NonNull<GcBox<()>>] {
        &[]
    }

    #[inline]
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Ok(value) = self.try_read() {  // Line 575
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

**與 GcRwLock 實作的一致性：**

bug34 中記錄的 GcRwLock 問題：
```rust
// sync.rs:600-604
impl<T: GcCapture + ?Sized> GcCapture for GcRwLock<T> {
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        if let Some(value) = self.inner.try_read() {  // 同樣問題
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

兩者使用相同的模式，導致相同的問題。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcCell, Trace};
use std::sync::RwLock;
use std::thread;
use std::time::Duration;

#[derive(Trace)]
struct Data {
    value: i32,
    gc_ptr: Option<Gc<Data>>,
}

fn main() {
    let cell = GcCell::new(RwLock::new(Data {
        value: 0,
        gc_ptr: None,
    }));
    
    // 執行緒持續持有寫鎖
    let writer = thread::spawn(move || {
        loop {
            {
                let mut guard = cell.write().unwrap();
                guard.value += 1;
                guard.gc_ptr = Some(Gc::new(Data {
                    value: guard.value,
                    gc_ptr: None,
                }));
            }
            thread::sleep(Duration::from_millis(1));
        }
    });
    
    // 嘗試觸發 GC
    for _ in 0..100 {
        rudo_gc::collect_full();
        thread::sleep(Duration::from_millis(10));
    }
    
    writer.join().unwrap();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

選項 1：使用讀取鎖並阻塞
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Ok(value) = self.read() {
        value.capture_gc_ptrs_into(ptrs);
    }
}
```

選項 2：記錄失敗而非靜默忽略
```rust
fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
    if let Ok(value) = self.try_read() {
        value.capture_gc_ptrs_into(ptrs);
    } else {
        tracing::warn!("Failed to capture GC pointers from RwLock - writer held lock");
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
與 GcRwLock 相同的問題模式。確保所有 GC 指標都能被掃描是基本要求。使用 `try_read()` 可能在高並發場景下遺漏指標。

**Rustacean (Soundness 觀點):**
這是 API 一致性問題。`std::sync::RwLock` 應該與 `GcRwLock` 有類似的行為，或者明確記錄這種差異。

**Geohot (Exploit 觀點):**
雖然利用難度較高，但如果攻擊者能夠控制時序，可能導致記憶體洩漏或不一致的 GC 狀態。

---

## 📌 與現有 Bug 的關係

- **bug34**: GcRwLock 使用 try_read() - 相同模式，不同類型
- **bug33**: GcMutex 缺少 GcCapture - 相關問題
- **bug28**: GcRwLock capture_gc_ptrs 返回空切片 - 相關問題
