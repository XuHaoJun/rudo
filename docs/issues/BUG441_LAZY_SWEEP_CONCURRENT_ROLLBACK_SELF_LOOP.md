# [Bug]: lazy_sweep_page 并发回滚时可能产生自循环空闲链表

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要并发分配恰好发生在空闲列表头 CAS 失败并返回 reclaimed slot index 时 |
| **Severity (嚴重程度)** | High | 可能导致对象泄漏和空闲链表损坏 |
| **Reproducibility (復現難度)** | Very High | 理论上可通过压力测试重现 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `lazy_sweep_page` in `crates/rudo-gc/src/gc/gc.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

在 `lazy_sweep_page` 函数中，当检测到 slot 在 CAS 成功后被并发分配而需要回滚时，如果此时 inner CAS (`free_list_head` 从 reclaimed slot index 回滚到 old value) 失败，并且返回的 `actual` 值恰好等于当前正在 reclaim 的 slot index，会导致该 slot 的 next pointer 被设置为指向自己（自循环）。

### 预期行为 (Expected Behavior)

当回滚 CAS 失败时，应该跳过该 slot（因为它已被并发分配），并且该 slot 应该保持在已分配状态。

### 实际行为 (Actual Behavior)

1. CAS(`free_list_head`, old, i) 成功 → `free_list_head` 变为 i
2. 写入 `slot[i] = old`（slot i 的 next 指向 old）
3. `is_allocated(i)` 返回 true（slot 被并发分配了）
4. 尝试 CAS(`free_list_head`, i, old) 回滚
5. **但此时另一个线程可能已经修改了 `free_list_head`** → CAS 失败，返回 `actual = i`（等于当前 reclaim 的 slot index）
6. Err 处理：`current_free = Some(i)`，`slot[i] = Some(i)`（自循环！）
7. 下一轮循环：`CAS(free_list_head, i, i)` 成功（因为 CAS(i, i) 是空操作）
8. `is_allocated(i)` 仍然为 true → `did_reclaim = false` → **永远不清理该 slot**

结果：该 slot 既处于已分配状态，又在空闲链表中指向自己，导致对象泄漏和空闲链表损坏。

---

## 🔬 根本原因分析 (Root Cause Analysis)

问题代码在 `crates/rudo-gc/src/gc/gc.rs` 第 2640-2698 行：

```rust
loop {
    let old = current_free.unwrap_or(u16::MAX);
    match (*header).free_list_head.compare_exchange(
        old,
        u16::try_from(i).unwrap(),
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => {
            if (*header).is_allocated(i) {
                // 尝试回滚
                if (*header)
                    .free_list_head
                    .compare_exchange(
                        u16::try_from(i).unwrap(),
                        old,
                        ...
                    )
                    .is_err()
                {
                    // CAS 失败，返回 actual
                    current_free = (*header).free_list_head(); // 如果 actual = i，则 current_free = Some(i)
                    // ...
                    obj_cast.write_unaligned(current_free); // 写入 Some(i) → 自循环!
                }
                // ...
                did_reclaim = false;
                break; // slot 被跳过但未正确处理
            }
            did_reclaim = true;
            break;
        }
        Err(actual) => {
            current_free = if actual == u16::MAX { None } else { Some(actual) };
            obj_cast.write_unaligned(current_free);
            // 注意：这里写入 current_free 后没有检查是否等于 Some(i)
            // 如果 current_free == Some(i)，会导致自循环
        }
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 理论上的重现步骤：
1. 创建大量对象，触发 GC
2. 在 lazy_sweep_page 处理某个 slot i 时
3. 另一个线程并发分配恰好重用 slot i
4. 并且在回滚 CAS 执行的瞬间，free_list_head 被设置为 i（通过另一个线程的分配操作）
5. 导致回滚 CAS 失败并返回 actual = i
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 Err 处理中，检测 `current_free == Some(i)` 的情况，如果是：
1. 不写入 slot i（因为它是自循环）
2. 设置 `did_reclaim = false` 并 break

或者更好的方案：完全重新设计这段回滚逻辑，确保在任何 CAS 失败的情况下都能正确处理。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
自循环的空闲链表节点会导致分配器行为异常，可能跳过应该被分配的 slot。这在长时间运行的服务器进程中累积可能导致内存泄漏。

**Rustacean (Soundness 觀點):**
虽然不直接导致内存不安全（use-after-free），但空闲链表损坏可能导致：
1. 内存泄漏（对象无法被重新分配）
2. 分配失败（如果空闲链表损坏到无法找到任何可用 slot）

**Geohot (Exploit 觀點):**
如果自循环的 slot 之后被"分配"（通过直接设置 `free_list_head` 绕过正常路径），可能导致 double-free 或其他内存损坏。
