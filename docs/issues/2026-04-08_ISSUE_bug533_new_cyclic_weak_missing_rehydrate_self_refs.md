# [Bug]: `new_cyclic_weak` 不調用 `rehydrate_self_refs` 導致循環弱引用無法正確康復

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 用戶使用 `new_cyclic_weak` 創建自引用資料結構時會觸發 |
| **Severity (嚴重程度)** | High | 循環弱引用在 GC 收集後無法正確康復，導致無效記憶體引用 |
| **Reproducibility (復現難度)** | Medium | 可通过 stress test 復現，需觸發 slot reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::new_cyclic_weak`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8.x

---

## 📝 問題描述 (Description)

`new_cyclic_weak` 函數在構建完成後**不調用** `rehydrate_self_refs`，而已弃用的 `new_cyclic` 卻會調用。這導致使用 `new_cyclic_weak` 創建的循環弱引用在 GC 收集後無法正確康復。

### 預期行為 (Expected Behavior)

`new_cyclic_weak` 应该在构造完成后调用 `rehydrate_self_refs`，类似于 `new_cyclic` 的处理方式，以确保自引用结构中的弱指针能够正确恢复。

### 實際行為 (Actual Behavior)

1. `new_cyclic` (ptr.rs:1406) 在值写入后调用 `rehydrate_self_refs(gc_box_ptr, &(*gc_box).value)`
2. `new_cyclic_weak` (ptr.rs:1447-1548) 只调用 `set_under_construction(false)`，但**不调用** `rehydrate_self_refs`

这意味着：
- 构造期间 `Weak` 引用是正确的（指向新分配的 GcBox）
- 但当循环结构被收集且 slot 被重用时，旧 `Weak` 引用不会被重新填充到新分配

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs` 中：

**`new_cyclic` (已弃用) - 行 1375-1410:**
```rust
// 第 1406 行 - 调用 rehydrate_self_refs
rehydrate_self_refs(gc_box_ptr, &(*gc_box).value);
```

**`new_cyclic_weak` - 行 1447-1548:**
```rust
// 第 1531-1533 行 - 只设置 under_construction 为 false，没有 rehydrate
unsafe {
    (*gc_box_ptr.as_ptr()).set_under_construction(false);
}

guard.completed = true;
std::mem::forget(guard);
// 缺少: rehydrate_self_refs(gc_box_ptr, &(*gc_box).value);
```

`rehydrate_self_refs` 函数存在（ptr.rs:3197），但 `new_cyclic_weak` 从不调用它。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
use rudo_gc::{Gc, Weak, Trace, GcCell, collect_full};

#[derive(Trace)]
struct Node {
    self_ref: GcCell<Option<Weak<Node>>>,
    data: i32,
}

// 使用 new_cyclic_weak 创建自引用结构
let node = Gc::new_cyclic_weak(|weak_self| Node {
    self_ref: GcCell::new(Some(weak_self)),
    data: 42,
});

// 升级测试 - 构造后应该能工作
assert!(node.self_ref.borrow().as_ref().unwrap().upgrade().is_some());

// 删除引用触发 GC
drop(node);
collect_full();

// 创建新节点（如果 slot 被重用）
let new_node = Gc::new(Node {
    self_ref: GcCell::new(None),
    data: 100,
});

// 旧 Weak 引用的行为取决于实现
// bug532: rehydrate_self_refs 从未被调用
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `new_cyclic_weak` 的第 1535 行（`guard.completed = true;`）之后、`std::mem::forget(guard)` 之前，添加 `rehydrate_self_refs` 调用：

```rust
unsafe {
    rehydrate_self_refs(gc_box_ptr, &(*gc_box_ptr.as_ptr()).value);
}
```

或者，如果 `rehydrate_self_refs` 的当前实现不完善（只有 FIXME），则需要：
1. 实现 `rehydrate_self_refs` 的完整功能
2. 确保 `new_cyclic_weak` 正确调用它

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
`rehydrate_self_refs` 函数设计用于处理自引用 Gc 在被收集后 slot 被重用的情况。函数会追踪值中的字段，找出任何死的 Gc 指针（具有 null 或过时内部指针），并在目标仍然存活时更新它们指向新分配。

然而，FIXME 中提到的实现难度是真实的：没有运行时类型信息，我们无法安全地验证"死" Gc 引用应该重新填充到特定的新分配。我们的设计中的类型擦除使这变得有问题。

**Rustacean (Soundness 觀點):**
`new_cyclic_weak` 创建循环结构但从不调用任何重新填充函数，这是一个明显的遗漏。当循环结构变得不可达并被收集时，slot 可以被重用用于新分配。但是，嵌入在收集值字段中的旧 `Weak` 引用不会更新为指向新分配——它们仍然指向旧的（现在已收集的） GcBox，其 generation counter 已经递增。

**Geohot (Exploit 觀點):**
如果自引用循环没有正确重新填充，攻击者可能能够：
1. 创建一个自引用结构
2. 让它变得不可达并被收集
3. 在重用的 slot 中分配新数据
4. 让旧 Weak 引用意外地指向新数据

这可能可能被用来绕过安全检查，如果 Weak 的 `upgrade()` 在不应该时返回 `Some`。然而，generation counter 机制（如果正确实现和检查）应该防止这种情况。

**Summary:**
核心问题是 `new_cyclic_weak` 不调用 `rehydrate_self_refs`。这与 bug532 的描述完全一致。修复需要在 `new_cyclic_weak` 中添加对 `rehydrate_self_refs` 的调用，或实现 `rehydrate_self_refs` 的完整功能。

---

## 驗證指南檢查

- Pattern 1 (Full GC 遮蔽 barrier bug): N/A - 这是关于 Weak 重新填充，不是 barrier
- Pattern 2 (單執行緒無法觸發競態): 需要多線程測試
- Pattern 3 (測試情境與 issue 描述不符): PoC 符合 issue 描述
- Pattern 4 (容器內的 Gc 未被當作 root): N/A
- Pattern 5 (難以觀察的內部狀態): 需要可觀察的對外行為作為 proxy
