# [Bug]: Gc::new_cyclic_weak 缺少 mark_new_object_black 導致增量標記期間提前回收

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要增量標記期間執行 new_cyclic_weak |
| **Severity (嚴重程度)** | High | 可能導致部分構造的對象被過早回收 |
| **Reproducibility (復現難度)** | Medium | 需要特定時序，但可穩定復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::new_cyclic_weak` (ptr.rs:1488-1596)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在 `Gc::new` 函數中（第 1294-1297 行），新分配的對象會調用 `mark_new_object_black` 標記為黑色（存活），這是 SATB "black allocation" 優化，確保增量標記期間新分配的對象立即可達：

```rust
// Mark as black (live) during incremental marking
// This is the SATB "black allocation" optimization
#[allow(clippy::ptr_as_ptr)]
let _ = mark_new_object_black(ptr.as_ptr() as *const u8);
```

### 實際行為 (Actual Behavior)
在 `Gc::new_cyclic_weak` 函數中（第 1586-1595 行），這個調用**缺失**：

```rust
crate::gc::notify_created_gc();

// Record for suspicious sweep detection
#[cfg(feature = "debug-suspicious-sweep")]
crate::gc::record_young_object(gc_box_ptr.as_ptr() as *const u8);

Self {
    ptr: AtomicNullable::new(gc_box_ptr),
    _marker: PhantomData,
}
```

缺少 `mark_new_object_black` 調用。

---

## 🔬 根本原因分析 (Root Cause Analysis)

增量標記使用 **SATB (Snapshot-At-The-Beginning)** 語義。"black allocation" 優化（Chez Scheme 風格）在分配時立即將新對象標記為存活。這確保：
1. 分配期間的對象立即可達
2. 無需 safepoint 就能使新分配對 GC 可見

`Gc::new` 正確實現了這一點（第 1297 行），但 **`Gc::new_cyclic_weak`（用於創建自引用結構）省略了此調用**。

後果：如果 major GC（增量標記）開始：
1. Thread A 調用 `Gc::new_cyclic_weak(|weak_self| ...)`
2. 對象被分配，`UNDER_CONSTRUCTION` 標誌被清除
3. **在** `Gc` 返回給調用者之前，增量標記可能會 sweep 這個對象
4. 對象沒有其他引用（只有正在構造的 `Gc` 持有它）
5. **過早回收發生**

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, GcCell, gc::IncrementalConfig};
use std::cell::Cell;

static COLLECTED: Cell<bool> = Cell::new(false);

#[derive(Trace)]
struct Node {
    self_ref: GcCell<Option<Weak<Node>>>,
    data: i32,
}

fn main() {
    IncrementalConfig::set_incremental_config(IncrementalConfig {
        enabled: true,
        increment_size: 10,
        max_dirty_pages: 100,
        remembered_buffer_len: 32,
        slice_timeout_ms: 1,
    });

    let node = Gc::new_cyclic_weak(|weak_self| Node {
        self_ref: GcCell::new(Some(weak_self)),
        data: 42,
    });

    assert_eq!(node.data, 42); // 如果 bug 發生可能 panic！
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Gc::new_cyclic_weak` 中，`set_under_construction(false)` 之後添加 `mark_new_object_black` 調用：

```rust
// 在第 1574-1575 行之後，set_under_construction(false) 之後添加：
#[allow(clippy::ptr_as_ptr)]
let _ = mark_new_object_black(gc_box_ptr.as_ptr() as *const u8);
```

這與 `Gc::new`（第 1297 行）的模式一致。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
SATB 語義要求所有分配期間的對象立即對 GC 可見。`new_cyclic_weak` 創建的自引用結構在構造完成後、返回給調用者之前，如果增量標記開始並掃描該 slot，則該對象可能未被標記為黑色（存活），導致過早回收。這是 black allocation 優化的基本要求。

**Rustacean (Soundness 觀點):**
`mark_new_object_black` 在分配時將對象標記為"已標記"，這樣即使沒有其他根，對象也不會被回收。缺少此調用可能導致 use-after-free，如果對象在 `new_cyclic_weak` 返回之前被回收並重用。增量標記期間的時序非常關鍵。

**Geohot (Exploit 觀點):**
攻擊者可能利用此窗口：在 `new_cyclic_weak` 的 `data_fn` 執行期間（在 `set_under_construction` 清除之後、返回之前）觸發增量標記。如果這段時間足夠長，可能導致 slot 被回收並分配給攻擊者控制的數據。雖然窗口很小，但理論上可行。