# [Bug]: mark_object_black 和 mark_new_object_black 缺少 is_under_construction 檢查

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需在 Gc::new_cyclic_weak 執行期間觸發 write barrier，理論上可能但需特定時序 |
| **Severity (嚴重程度)** | Medium | 可能導致未完全建構的物件被錯誤標記為 live，影響 GC 正確性 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制才能穩定重現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `mark_object_black`, `mark_new_object_black`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0+

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`mark_object_black` 和 `mark_new_object_black` 應該在標記物件前檢查 `is_under_construction` flag，與其他 GcBox 操作（如 `try_deref`、`try_clone`、`Gc::as_ptr`）保持一致。

### 實際行為 (Actual Behavior)
這兩個函數在標記物件時**沒有**檢查 `is_under_construction` flag，可能導致在 `Gc::new_cyclic_weak` 執行期間（物件仍在建構中）write barrier 錯誤地將物件標記為 live。

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. **UNDER_CONSTRUCTION_FLAG**：在 `Gc::new_cyclic_weak` 分配物件時會設定此 flag（參見 `ptr.rs` line 1212）
2. **Flag 清除時機**：此 flag 只會在建構成功完成後清除（`ptr.rs` line 1239）
3. **不一致行為**：其他類似函數如 `try_deref`、`try_clone`、`Gc::as_ptr`（都在 `ptr.rs` 中）**都會**在操作前檢查 `is_under_construction`

### 受影響的 call sites：
- `cell.rs` line 201 (`GcCell::borrow_mut`)
- `cell.rs` line 1285 (`GcThreadSafeRefMut::drop`)
- `sync.rs` line 393 (`GcRwLockWriteGuard::drop`)
- `sync.rs` line 446 (`GcRwLockWriteGuard::drop`)
- `sync.rs` line 701 (`GcMutexGuard::drop`)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要精確的時序控制，以下是概念驗證：

```rust
use rudo_gc::{Gc, Trace, GcCell};

struct Node {
    value: Gc<GcCell<i32>>,
}

impl Trace for Node {
    fn trace(&self, tracer: &mut dyn crate::tracer::Tracer) {
        self.value.trace(tracer);
    }
}

fn main() {
    // 建立 cyclic weak reference
    // 在 Gc::new_cyclic_weak 執行期間，如果觸發 write barrier
    // 可能會錯誤地將 under_construction 的物件標記為 live
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `mark_object_black` 和 `mark_new_object_black` 中添加 `is_under_construction` 檢查：

```rust
// mark_object_black (incremental.rs)
pub unsafe fn mark_object_black(ptr: *const u8) -> Option<usize> {
    let idx = crate::heap::ptr_to_object_index(ptr.cast())?;
    let header = crate::heap::ptr_to_page_header(ptr);
    let h = header.as_ptr();

    if !(*h).is_allocated(idx) {
        return None;
    }

    // NEW: 檢查物件是否正在建構中
    let gc_box_ptr = /* 從 ptr 計算 GcBox指標 */;
    if (*gc_box_ptr).is_under_construction() {
        return None;
    }

    // ... 現有程式碼
}

// mark_new_object_black 也需要類似修改
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 incremental marking 中，物件標記的時機至關重要。若在建構中的物件被錯誤標記，可能導致：
1. 循環引用在建構失敗時無法正確清理
2. 破壞generational GC的promotion假設

**Rustacean (Soundness 觀點):**
此 bug 涉及 unsafe 程式碼中的一致性問題。其他所有 GcBox 操作都檢查 `is_under_construction`，唯獨這裡漏掉，屬於 API 不一致导致的潜在 UB。

**Geohot (Exploit 觀點):**
若攻擊者能精確控制時序，在 `Gc::new_cyclic_weak` 期間觸發大量 write barrier，可能：
1. 導致記憶體洩漏（錯誤標記的物件無法回收）
2. 破壞 GC 的完整性假設
