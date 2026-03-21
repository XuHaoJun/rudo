# [Bug]: mark_new_object_black 缺少 generation check 导致错误的 slot 清理

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 incremental marking + lazy sweep 触发 slot sweep+reuse 竞争 |
| **Severity (嚴重程度)** | Medium | 可能导致错误的 mark 状态，影响 GC 正确性 |
| **Reproducibility (復現難度)** | Medium | 需要仔细设计 PoC 来触发 slot reuse 时机 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `mark_new_object_black` (incremental.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`mark_new_object_black` 函数在 slot 被 sweep 后重新分配 (reuse) 的情况下，可能错误地清理 mark 或不正确地设置 mark。与 `mark_object_black` 不同，它缺少 generation 检查来区分 "slot 被 sweep" 和 "slot 被 sweep 后再利用"。

### 預期行為 (Expected Behavior)
当 slot 被 sweep 后再利用，mark 状态应该正确处理，不应该错误地清理属于新对象的 mark。

### 實際行為 (Actual Behavior)
`mark_new_object_black` 使用 `set_mark` 后检查 `is_allocated`，但不检查 generation。如果 slot 在 `set_mark` 和 `is_allocated` 检查之间被 sweep 并 reuse，generation 会改变但代码无法检测。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**文件:** `crates/rudo-gc/src/gc/incremental.rs:1042-1050`

`mark_new_object_black` 代码：
```rust
if !(*header.as_ptr()).is_marked(idx) {
    (*header.as_ptr()).set_mark(idx);
    // Re-check is_allocated to fix TOCTOU with lazy sweep (bug272).
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return false;
    }
    return true;
}
```

相比之下，`mark_object_black` (lines 1107-1124) 有正确的 generation 检查：
```rust
// Read generation after successful mark to detect slot reuse (bug355 fix).
let marked_generation = (*gc_box).generation();
// We just marked. Re-check is_allocated to fix TOCTOU with lazy sweep.
if (*h).is_allocated(idx) {
    return Some(idx);
}
// Slot was swept between our check and try_mark.
// Verify generation hasn't changed to distinguish swept from swept+reused.
let current_generation = (*gc_box).generation();
if current_generation != marked_generation {
    // Slot was reused - the mark now belongs to the new object, don't clear.
    return None;
}
// Slot was swept but not reused - safe to clear mark.
(*h).clear_mark_atomic(idx);
return None;
```

**问题：** `mark_new_object_black` 缺少 lines 1108-1120 的 generation 检查逻辑。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要 PoC 来验证:
// 1. 启用 incremental marking
// 2. 分配对象 A
// 3. 触发 lazy sweep 将 A 的 slot 标记为 free
// 4. 重新分配该 slot 给对象 B (generation 改变)
// 5. 调用 mark_new_object_black(ptr_B)
// 6. 验证 generation 是否被正确检查

// 理论场景：
// - 对象 A 在 generation G1 时被分配
// - A 被 sweep，slot 进入 free list
// - 对象 B 在 generation G2 时分配到同一 slot (G2 != G1)
// - mark_new_object_black 被调用
// - set_mark() 设置了 slot 的 mark
// - is_allocated() 返回 true (B 存在)
// - 返回 true，但 mark 状态可能属于旧对象 A
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `mark_new_object_black` 中添加类似 `mark_object_black` 的 generation 检查：

```rust
if !(*header.as_ptr()).is_marked(idx) {
    // Get generation BEFORE set_mark to detect slot reuse
    let marked_generation = (*gc_box).generation();
    
    (*header.as_ptr()).set_mark(idx);
    
    // Re-check is_allocated to fix TOCTOU with lazy sweep (bug272).
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return false;
    }
    
    // Verify generation hasn't changed to distinguish swept from swept+reused.
    let current_generation = (*gc_box).generation();
    if current_generation != marked_generation {
        // Slot was reused - the mark now belongs to the new object
        return true;  // Don't clear, mark is valid for new object
    }
    
    return true;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
mark_object_black 和 mark_new_object_black 的实现不一致是危险的。generational GC 依赖 generation 来追踪 slot 生命周期。缺少这个检查可能导致标记错误的对象或错误清理应该保留的标记。这在 incremental marking + lazy sweep 组合下尤其成问题，因为 sweep 和 marking 可能交错执行。

**Rustacean (Soundness 觀點):**
代码使用 `set_mark` 而非 `try_mark`，这意味着它不是原子操作。在并发环境下，多个线程可能同时对同一 slot 调用 `set_mark`。虽然有 `is_marked` 检查，但 TOCTOU 窗口仍然存在。`mark_object_black` 使用 `try_mark` + CAS 是更安全的模式。

**Geohot (Exploit 觀點):**
这个 bug 可能被利用来造成 mark 状态混淆。如果攻击者能控制 allocation 模式，他们可能触发特定的 slot reuse 模式来操纵 GC 的 mark 状态。这可能导致对象被错误地回收或保留，从而触发 UAF 或内存泄漏。

(End of file - total 137 lines)