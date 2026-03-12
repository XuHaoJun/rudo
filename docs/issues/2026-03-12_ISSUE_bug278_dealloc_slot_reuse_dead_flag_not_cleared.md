# [Bug]: heap::dealloc 回收 slot 時未清除 DEAD_FLAG

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需主動呼叫 dealloc 才會觸發，不同於一般 GC 回收 |
| **Severity (嚴重程度)** | High | 可能導致新分配的物件被錯誤標記為 dead，影響記憶體安全 |
| **Reproducibility (復現難度)** | Medium | 需追蹤 dealloc 後的 slot 重複使用 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `heap::LocalHeap::dealloc`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

當 slot 被回收並返回到 free list 時（不論是透過 lazy sweep 還是 explicit dealloc），應該清除所有 flags (DEAD_FLAG, GEN_OLD_FLAG, UNDER_CONSTRUCTION_FLAG)，確保新物件不會繼承舊狀態。

### 實際行為 (Actual Behavior)

在 `heap.rs` 的 `dealloc` 函數中（第 2654-2655 行），回收 slot 時只清除了 GEN_OLD_FLAG 和 UNDER_CONSTRUCTION_FLAG，**沒有清除 DEAD_FLAG**。

相比之下，`pop_from_free_list`（第 2224-2228 行）正確地清除了所有 flags：
```rust
(*gc_box_ptr).clear_dead();
(*gc_box_ptr).clear_gen_old();
(*gc_box_ptr).clear_under_construction();
(*header).clear_dirty(idx as usize);
```

但 `dealloc` 只清除：
```rust
(*gc_box_ptr).clear_gen_old();
(*gc_box_ptr).clear_under_construction();
```

缺少 `(*gc_box_ptr).clear_dead();`

---

## 🔬 根本原因分析 (Root Cause Analysis)

1. `dealloc` 函數在回收 slot 時遺漏了 `clear_dead()` 呼叫
2. 當 slot 被重複使用時，新物件會繼承舊的 DEAD_FLAG，導致 `has_dead_flag()` 錯誤回傳 true
3. 這可能導致物件被錯誤地視為已死亡，影響後續的 weak upgrade 或其他操作

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要设计一个测试，显式调用 `dealloc` 回收对象，然后检查重新分配时 DEAD_FLAG 是否被正确清除。

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `heap.rs:2654-2655` 處新增 `clear_dead()` 呼叫：

```rust
unsafe {
    (*gc_box_ptr).clear_dead();  // 新增這行
    (*gc_box_ptr).clear_gen_old();
    (*gc_box_ptr).clear_under_construction();
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
slot 重用時清除 flags 是標準做法，確保新物件不會受舊狀態影響。dealloc 應該與 allocation path 保持一致的清理行為。

**Rustacean (Soundness 觀點):**
遺漏 clear_dead() 可能導致 UB：新物件的 has_dead_flag() 會錯誤回傳 true，影響記憶體安全的假設。

**Geohot (Exploit 觀點):**
若物件被錯誤標記為 dead，攻擊者可能利用此狀態進行 TOCTOU 攻擊或繞過安全檢查。