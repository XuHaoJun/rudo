# [Bug]: mark_new_object_black 缺少 generation check（bug358 修復未合併）

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 incremental marking + lazy sweep 触发 slot sweep+reuse 竞争 |
| **Severity (嚴重程度)** | Medium | 可能导致错误的 mark 状态，影响 GC 正确性 |
| **Reproducibility (復現難度)** | Medium | 需要仔细设计 PoC 来触发 slot reuse 时机 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/incremental.rs`, `mark_new_object_black`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.0

---

## 📝 問題描述 (Description)

`mark_new_object_black` 函数在 slot 被 sweep 后重新分配 (reuse) 的情况下，可能错误地处理 mark 状态。与 `mark_object_black` 不同，它缺少 generation 检查来区分 "slot 被 sweep" 和 "slot 被 sweep 后再利用"。

**bug358 修复 commit (b118877) 未合并到当前 HEAD。**

### 預期行為 (Expected Behavior)
- `mark_new_object_black` 应该在 `set_mark` 后检查 generation 是否改变
- 如果 generation 改变，说明 slot 被 reuse，mark 属于新对象，不应清除

### 實際行為 (Actual Behavior)
`mark_new_object_black` 捕获 `marked_generation` 但从未使用它来验证 slot 是否被 reuse。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**文件:** `crates/rudo-gc/src/gc/incremental.rs:1076-1084`

`mark_new_object_black` 代码：
```rust
if !(*header.as_ptr()).is_marked(idx) {
    let marked_generation = (*gc_box).generation();  // 捕获但未使用！
    (*header.as_ptr()).set_mark(idx);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return false;
    }
    return true;  // 缺少 generation 检查！
}
```

对比 `mark_object_black` (lines 1140-1162) 有正确的 generation 检查：
```rust
Ok(true) => {
    let marked_generation = (*gc_box).generation();
    if (*h).is_allocated(idx) {
        let current_generation = (*gc_box).generation();
        if current_generation != marked_generation {
            return None;  // Slot reused - mark belongs to new object
        }
        return Some(idx);
    }
    // ...
}
```

**问题：** `marked_generation` 被捕获但从未比较，失去了检测 slot reuse 的能力。

---

## 💣 重現步驟 / 概念驗證 (PoC)

```rust
// 理论场景 (需要并发 GC 才能稳定触发):
// 1. 对象 A 在 generation G1 时被分配，调用 mark_new_object_black
// 2. A 被 lazy sweep 回收，slot 进入 free list
// 3. 对象 B 在 generation G2 时分配到同一 slot (G2 != G1)
// 4. mark_new_object_black 被调用
// 5. set_mark() 设置了 slot 的 mark
// 6. is_allocated() 返回 true (B 存在)
// 7. 返回 true，但 generation 未检查！
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

在 `mark_new_object_black` 中添加 generation 检查：

```rust
if !(*header.as_ptr()).is_marked(idx) {
    let marked_generation = (*gc_box).generation();
    (*header.as_ptr()).set_mark(idx);
    if !(*header.as_ptr()).is_allocated(idx) {
        (*header.as_ptr()).clear_mark_atomic(idx);
        return false;
    }
    // Verify generation hasn't changed to distinguish swept from swept+reused.
    let current_generation = (*gc_box).generation();
    if current_generation != marked_generation {
        // Slot was reused - the mark now belongs to the new object
        return true;
    }
    return true;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
- generational GC 依赖 generation 来追踪 slot 生命周期
- 缺少这个检查可能导致标记错误的对象，在 incremental marking + lazy sweep 组合下尤其成问题

**Rustacean (Soundness 觀點):**
- 代码行为与 `mark_object_black` 不一致
- `marked_generation` 被捕获但从未使用，这是死代码

**Geohot (Exploit 觀點):**
- 如果攻击者能控制 allocation 模式，可能触发特定的 slot reuse 模式来操纵 GC 的 mark 状态

---

## 驗證

```bash
git merge-base --is-ancestor b118877 HEAD && echo "In HEAD" || echo "NOT in HEAD"
# Output: NOT in HEAD
```

commit b118877 的修复未合并到当前 HEAD。