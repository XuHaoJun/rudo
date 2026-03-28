# [Bug]: incremental_write_barrier large object path missing second is_allocated check (TOCTOU)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確時序：slot sweep 在 early return check 和 record_in_remembered_buffer 之间发生 |
| **Severity (嚴重程度)** | High | 可能导致 remembered set 包含无效 slot，GC 扫描错误对象 |
| **Reproducibility (重現難度)** | High | 需要并发场景：lazy sweep 与 mutator 并发执行 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `heap.rs::incremental_write_barrier` (lines 3157-3217)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.18

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`incremental_write_barrier` 应该在 early return 之后、调用 `record_in_remembered_buffer` 之前，对所有代码路径（包括大对象路径）执行第二次 `is_allocated` 检查。

这与 `gc_cell_validate_and_barrier` 和 `unified_write_barrier` 保持一致，这两个函数都对两个代码路径执行第二次检查。

### 實際行為 (Actual Behavior)

在 `incremental_write_barrier` 中：

**Large object path (lines 3157-3176):**
```rust
// Line 3168: First is_allocated check
if !(*h_ptr).is_allocated(0) {
    return;
}
// ... read has_gen_old_flag, early return check ...
// Line 3176: Return (h_ptr, 0)
// NO second is_allocated check after early return!
```

**Normal page path (lines 3177-3210):**
```rust
// Line 3199: First is_allocated check
if !(*h.as_ptr()).is_allocated(index) {
    return;
}
// ... read has_gen_old_flag, early return check ...
// Line 3209: Return (h, index)
```

**After both paths (lines 3212-3215):**
```rust
// Second is_allocated check - 但大对象路径在 line 3176 就返回了，永远不会到达这里！
if !(*header.as_ptr()).is_allocated(index) {
    return;
}
heap.record_in_remembered_buffer(header);
```

问题是：Large object path 在 line 3176 直接返回，永远不会到达 line 3212-3215 的第二次检查。

### 受影響的函數對比

| 函數 | Large object 第一次檢查 | Large object 第二次檢查 | Normal page 第一次檢查 | Normal page 第二次檢查 |
|------|------------------------|------------------------|------------------------|------------------------|
| `gc_cell_validate_and_barrier` | Line 2935 | Line 3022 | Line 2988 | Line 3022 |
| `unified_write_barrier` | Line 3071 | Line 3112 | Line 3099 | Line 3112 |
| `incremental_write_barrier` | Line 3168 | **缺失** | Line 3199 | Line 3212-3215 |

---

## 🔬 根本原因分析 (Root Cause Analysis)

TOCTOU (Time-Of-Check-Time-Of-Use) 场景：

1. Mutator 执行 `incremental_write_barrier` 对大对象
2. Line 3168: 第一次 `is_allocated(0)` 检查 → 通过（slot 仍分配）
3. Line 3172: 读取 `has_gen_old_flag()`
4. Line 3173-3174: Early return 检查 → **不返回**（因为条件不满足）
5. **此时**：Lazy sweep 回收该 slot 并分配给新对象
6. Line 3176: 返回 `(h_ptr, 0)`
7. Line 3212-3215: 第二次 `is_allocated(index)` 检查 → **不执行**（大对象路径提前返回）
8. Line 3217: `record_in_remembered_buffer(header)` → 对**已回收的 slot** 记录！

后果：
- `record_in_remembered_buffer` 可能对无效/回收的 slot 进行操作
- 可能导致 GC 扫描到错误的对象
- 违反 memory safety

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要并发测试：
1. 启用 lazy sweep
2. Thread A：分配大对象，触发 `incremental_write_barrier`
3. Thread B：执行 lazy sweep，回收该大对象 slot
4. 观察 `record_in_remembered_buffer` 是否在回收的 slot 上操作

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `incremental_write_barrier` 的 large object path 添加第二次 `is_allocated` 检查：

```rust
// Large object path
if !(*h_ptr).is_allocated(0) {
    return;
}
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation.load(Ordering::Acquire) == 0 && !has_gen_old {
    return;
}
// FIX bug430: 添加第二次 is_allocated 检查
if !(*h_ptr).is_allocated(0) {
    return;
}
(NonNull::new_unchecked(h_ptr), 0_usize)
```

或者，更簡潔的方式是將第二次檢查移到 if-else 結構之外，確保兩個路徑都執行：

```rust
let (header, index) = /* ... */;

// 確保大对象路径也执行第二次检查
if header.as_ptr() == std::ptr::null_mut() || !(*header.as_ptr()).is_allocated(index) {
    return;
}

heap.record_in_remembered_buffer(header);
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
TOCTOU 竞争条件在 GC 中特别危险，因为 slot 回收和引用追踪之间的时序非常紧密。大对象路径缺少第二次检查使得 remembered set 可能包含已回收的 slot，导致 GC 扫描到无效对象。

**Rustacean (Soundness 觀點):**
这是一个 memory safety 问题。如果 `record_in_remembered_buffer` 在已回收的 slot 上操作，可能导致未定义行为。参考 `gc_cell_validate_and_barrier` 和 `unified_write_barrier` 的实现，它们都对两个路径执行第二次检查。

**Geohot (Exploit 觀點):**
攻击者可能通过控制分配时序，在 `record_in_remembered_buffer` 执行时使 slot 已被回收。这可能导致 GC 扫描到攻击者控制的数据。

---

## 驗證記錄

**驗證日期:** 2026-03-27
**驗證人員:** opencode

### 驗證結果

通過代碼比對確認差異：

1. `gc_cell_validate_and_barrier`: 第二次檢查位於 line 3022，兩個路徑都執行
2. `unified_write_barrier`: 第二次檢查位於 line 3112，兩個路徑都執行
3. `incremental_write_barrier`: 第二次檢查位於 line 3212-3215，但 large object 路徑在 line 3176 直接返回，**不會執行**第二次檢查

**Status: Open** - 需要修復。

---

## Resolution (2026-03-28)

**Outcome:** Already fixed in `crates/rudo-gc/src/heap.rs` `incremental_write_barrier`.

**Verification:** Static review of current `incremental_write_barrier` (approx. lines 3157–3221):

1. **Large-object branch:** After the generational early-exit (`generation == 0 && !has_gen_old`), a **second** `is_allocated(0)` runs before building `(header, index)` (lines 3176–3177), matching the suggested fix in this issue.
2. **Merged path:** After both branches, `is_allocated(index)` runs again before `record_in_remembered_buffer` (lines 3215–3218), so the large-object path is not exempt from the final check.

This aligns with bug364 (common second check) and the large-object-specific second check. `cargo test -p rudo-gc --test incremental_marking --test incremental_integration -- --test-threads=1` passed.
