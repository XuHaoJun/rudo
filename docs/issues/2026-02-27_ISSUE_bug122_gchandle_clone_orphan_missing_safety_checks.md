# [Bug]: GcHandle::clone 缺少對 orphan root 的安全檢查

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 僅在 origin thread 已終止且嘗試 clone GcHandle 時觸發 |
| **Severity (嚴重程度)** | High | 可能導致在已drop/dropping/under construction的對象上增加引用計數 |
| **Reproducibility (復現難度)** | Medium | 需要多執行緒場景 + origin thread 終止 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::clone`, `clone_orphan_root_with_inc_ref`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
當 clone 一個 `GcHandle` 時，無論是 origin thread 仍存活還是已終止（變成 orphan root），都應該檢查對象是否為 dead、dropping 或 under construction 狀態。

### 實際行為 (Actual Behavior)
在 `GcHandle::clone()` 中：
- 當 origin thread 仍存活：有進行安全檢查 (lines 375-382 in cross_thread.rs)
- 當 origin thread 已終止（orphan）：調用 `heap::clone_orphan_root_with_inc_ref()`，但該函數**沒有**進行這些安全檢查

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/handles/cross_thread.rs` 的 `GcHandle::clone()`:

1. **非 orphan 路徑** (lines 366-382) - 有檢查:
```rust
let gc_box = &*self.ptr.as_ptr();
assert!(
    !gc_box.has_dead_flag()
        && gc_box.dropping_state() == 0
        && !gc_box.is_under_construction(),
    "GcHandle::clone: cannot clone a dead, dropping, or under construction GcHandle"
);
gc_box.inc_ref();
```

2. **Orphan 路徑** (lines 354-364) - 缺少檢查:
```rust
let (new_id, ok) = heap::clone_orphan_root_with_inc_ref(
    self.origin_thread,
    self.handle_id,
    self.ptr.cast::<GcBox<()>>(),
);
```

`clone_orphan_root_with_inc_ref()` (heap.rs:234-250) 直接調用 `inc_ref()` 而不檢查對象狀態。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要 Miri 或多執行緒測試
// 1. 在 thread A 創建 GcHandle
// 2. 終止 thread A
// 3. 在 thread B 嘗試 clone 該 GcHandle (會走 orphan 路徑)
// 4. 如果對象已經是 dead/dropping/under construction，inc_ref 會在錯誤的對象上增加計數
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `clone_orphan_root_with_inc_ref()` 中添加安全檢查，或在調用前進行檢查:

```rust
let (new_id, ok) = heap::clone_orphan_root_with_inc_ref(
    self.origin_thread,
    self.handle_id,
    self.ptr.cast::<GcBox<()>>(),
);

// 添加檢查
unsafe {
    let gc_box = &*self.ptr.as_ptr();
    assert!(
        !gc_box.has_dead_flag()
            && gc_box.dropping_state() == 0
            && !gc_box.is_under_construction(),
        "GcHandle::clone: cannot clone a dead, dropping, or under construction GcHandle (orphan)"
    );
}
```

或者在 `clone_orphan_root_with_inc_ref()` 內部添加檢查。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 clone 時增加 ref count 應該確保對象是有效的。如果對象已經被標記為 dead 或正在 dropping，incremented ref count 可能導致記憶體管理錯誤，例如對象被 sweep 後仍然有殘留的引用。

**Rustacean (Soundness 觀點):**
這不是傳統意義上的 UB，但可能導致 use-after-free 或記憶體洩漏。當對象已經被標記為 dead flag 時增加 ref count，會造成不一致的狀態。

**Geohot (Exploit 觀點):**
如果在對象處於特定狀態時進行 clone，可能會繞過 GC 的安全檢查。雖然此時不太可能有記憶體安全問題，但這是潛在的攻擊面。

---

## Resolution (2026-03-01)

**Outcome:** Fixed.

**Fix location:** `crates/rudo-gc/src/heap.rs` — `clone_orphan_root_with_inc_ref()`

Added the same three safety assertions that the non-orphan path uses (lines 376–381 of `cross_thread.rs`), placed **before** the new orphan entry is inserted and before `inc_ref()` is called. The checks run while the orphan lock is held (preventing TOCTOU with concurrent removal):

```rust
unsafe {
    let gc_box = &*ptr.as_ptr();
    assert!(
        !gc_box.has_dead_flag()
            && gc_box.dropping_state() == 0
            && !gc_box.is_under_construction(),
        "GcHandle::clone: cannot clone a dead, dropping, or under construction GcHandle (orphan)"
    );
}
```

Full test suite passes with no regressions (`bash test.sh`). Clippy clean.
