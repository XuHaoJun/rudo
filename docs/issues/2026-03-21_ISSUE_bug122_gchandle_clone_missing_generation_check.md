# [Bug]: GcHandle::clone() 缺少 generation 檢查可能導致 slot reuse 問題

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發 GC 和 slot reuse 才能觸發 |
| **Severity (嚴重程度)** | Medium | 導致記憶體洩漏，不是 UAF |
| **Reproducibility (復現難度)** | Medium | 需要精確的時序控制 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::clone()`, `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcHandle::clone()` 在 TCB path 中，於 `inc_ref()` 之前檢查 `is_allocated`，但沒有檢查 `generation` 來偵測 slot reuse。

### 預期行為
`clone()` 應該像 `downgrade()` 一樣，在 `inc_ref()` 之前取得 `generation`，並在之後驗證 `generation` 未改變，以偵測 slot reuse。

### 實際行為
`clone()` 只檢查 `is_allocated`，不檢查 `generation`：
```rust
// Check is_allocated before inc_ref
if let Some(idx) = ptr_to_object_index(...) {
    let header = ptr_to_page_header(...);
    assert!((*header.as_ptr()).is_allocated(idx), ...);
}
(*self.ptr.as_ptr()).inc_ref();  // No generation check!
let new_id = roots.allocate_id();
roots.strong.insert(new_id, ...);
```

對比 `downgrade()` (有 generation 檢查，bug351 fix)：
```rust
// Get generation BEFORE inc_weak to detect slot reuse (bug351).
let pre_generation = (*self.ptr.as_ptr()).generation();
(*self.ptr.as_ptr()).inc_weak();
// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != (*self.ptr.as_ptr()).generation() {
    (*self.ptr.as_ptr()).dec_weak();
    ...
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `handles/cross_thread.rs:580-604` 的 `clone()` 實作中：

1. 檢查 `is_allocated` (lines 593-600)
2. 呼叫 `inc_ref()` (line 601) - **沒有 generation 檢查**
3. 插入新 root 到 `roots.strong` (line 604)

如果 slot 在步驟 1 和 2 之間被 sweep 並重用：
1. 舊 `GcBox` 被釋放
2. 新 `GcBox` 被分配在同一個 slot，generation 增加
3. `inc_ref()` 在新 `GcBox` 上執行
4. 新 `GcBox` 的 `ref_count` 從 1 變成 2
5. 新 root 被插入到 `roots.strong`
6. 當所有參考被 drop，`dec_ref()` 從 2 變成 1（不是 0）
7. `GcBox` **不會被釋放** - 記憶體洩漏！

`downgrade()` 在 bug351 被修復時添加了 generation 檢查，但 `clone()` 沒有獲得相同的修復。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發 GC 和精確時序控制，難以稳定重現。理論場景：

```rust
// 理論 PoC - 需要並發 GC
use rudo_gc::{Gc, Trace};

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    let handle = std::thread::spawn(|| {
        let gc = Gc::new(Data { value: 42 });
        gc.cross_thread_handle()
    }).join().unwrap();

    // 如果在 clone() 期間 slot 被 sweep 並重用
    // 可能導致記憶體洩漏
    let _cloned = handle.clone();
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `clone()` 的 TCB path 中添加 generation 檢查，與 `downgrade()` 一致：

```rust
unsafe {
    // Get generation BEFORE inc_ref to detect slot reuse.
    let pre_generation = (*self.ptr.as_ptr()).generation();

    (*self.ptr.as_ptr()).inc_ref();

    // Verify generation hasn't changed - if slot was reused, undo inc_ref.
    if pre_generation != (*self.ptr.as_ptr()).generation() {
        (*self.ptr.as_ptr()).dec_ref();
        panic!("GcHandle::clone: slot was reused during clone");
    }
}
```

同樣需要檢查 orphan path（透過 `clone_orphan_root_with_inc_ref`）是否也有相同問題。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是並發 GC 和 slot reuse 交互的經典問題。generation 機制是專門設計來偵測這個問題的。`downgrade()` 已經有這個檢測（bug351），`clone()` 應該有一樣的保護。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題（不會導致 UAF），但是記憶體安全問題（記憶體洩漏）。雖然洩漏的 `GcBox` 無法被訪問，但記憶體永遠不會被回收。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制 GC 時機，可能可以觸發這個 race 導致記憶體洩漏，進而造成記憶體耗盡攻擊。但需要精確控制時序，難度較高。
