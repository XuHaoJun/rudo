# [Bug]: is_generational_barrier_active() returns false when incremental marking disabled, breaking GcRwLock/GcThreadSafeCell barriers

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | When user disables incremental marking |
| **Severity (嚴重程度)** | Critical | Use-after-free during minor collections |
| **Reproducibility (復現難度)** | Medium | Requires disabling incremental marking |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcRwLock`, `GcThreadSafeCell`, `is_generational_barrier_active`
- **OS / Architecture:** Linux x86_64
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

當用戶禁用 incremental marking 時，`is_generational_barrier_active()` 返回 `false`，導致 `GcRwLock` 和 `GcThreadSafeCell` 的 generational write barrier 不會觸發。

### 預期行為
- Generational barrier 應該在所有時間都可用，無論 incremental marking 是否啟用
- Minor collections 應該追蹤 OLD→YOUNG 引用

### 實際行為
- `is_generational_barrier_active()` 檢查 `state.enabled.load(Ordering::Relaxed)`
- 當 `enabled = false` 時，函數返回 `false`
- `GcRwLock::trigger_write_barrier()` 和 `GcThreadSafeCell::trigger_write_barrier()` 使用 `is_generational_barrier_active() || is_incremental_marking_active()`
- 當兩者都返回 false 時，barrier 不會觸發

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc/incremental.rs:472-477`:
```rust
pub fn is_generational_barrier_active() -> bool {
    let state = IncrementalMarkState::global();
    state.enabled.load(Ordering::Relaxed) && !state.fallback_requested()
}
```

問題：
1. 當用戶調用 `set_incremental_config(IncrementalConfig { enabled: false, .. })` 禁用 incremental marking
2. `is_generational_barrier_active()` 返回 `false`（因為 `enabled = false`）
3. `is_incremental_marking_active()` 也返回 `false`（因為 phase 是 Idle）
4. 導致 `GcRwLock` 和 `GcThreadSafeCell` 的 barrier 不會觸發
5. Minor collections 無法追蹤 OLD→YOUNG 引用，可能導致錯誤回收

相比之下，`GcCell::borrow_mut()` 使用不同的路徑：
- 直接調用 `gc_cell_validate_and_barrier()`，其內部檢查 `GEN_OLD_FLAG`
- 不依賴 `is_generational_barrier_active()`

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, GcRwLock, Trace, collect_full, set_incremental_config, IncrementalConfig};

#[derive(Trace)]
struct Data {
    value: i32,
}

fn main() {
    // 禁用 incremental marking
    set_incremental_config(IncrementalConfig {
        enabled: false,
        ..Default::default()
    });

    // 創建 GcRwLock 包裝的 OLD 對象
    let lock = Gc::new(GcRwLock::new(Data { value: 42 }));
    
    // 觸發 minor collection 將其 promote 到 old gen
    collect_full();
    
    // 分配新的 young 對象
    let young = Gc::new(Data { value: 100 });
    
    // 通過 GcRwLock 建立 OLD→YOUNG 引用
    // 這裡 barrier 不會觸發（因為 enabled = false）
    {
        let mut guard = lock.write();
        // 假設這裡有辦法存儲 young 對象的引用
    }
    
    // 再次 collect (minor GC)
    // Young 對象可能會被錯誤回收，因為 barrier 沒有記錄 OLD→YOUNG 引用
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：修改 `is_generational_barrier_active()` 邏輯

```rust
pub fn is_generational_barrier_active() -> bool {
    let state = IncrementalMarkState::global();
    // 總是返回 true，除非明確請求 fallback
    // Generational barrier 應該獨立於 incremental marking 運行
    !state.fallback_requested()
}
```

### 方案 2：修改調用點

在 `GcRwLock::trigger_write_barrier()` 和 `GcThreadSafeCell::trigger_write_barrier()` 中，直接檢查對象的 `GEN_OLD_FLAG`，而不是通過 `is_generational_barrier_active()`。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Generational barrier 應該獨立於 incremental marking。這是兩個不同的概念：
- Incremental marking: 將 major GC 分解為多個 slice
- Generational GC: 追蹤 OLD→YOUNG 引用用於 minor collections

當禁用 incremental marking 時，generational GC 仍然應該正常工作。

**Rustacean (Soundness 觀點):**
這是一個正確性問題。當 barrier 失敗時，可能導致 use-after-free。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以通過禁用 incremental marking 來觸發 bug，導致內存錯誤。

---

## Resolution (2026-02-26)

**Outcome:** Fixed.

Removed the `state.enabled` check from `is_generational_barrier_active()` in `gc/incremental.rs`. The generational barrier is now active whenever `!state.fallback_requested()`, independent of incremental marking. GcRwLock and GcThreadSafeCell barriers now correctly record OLD→YOUNG references even when incremental marking is disabled.
