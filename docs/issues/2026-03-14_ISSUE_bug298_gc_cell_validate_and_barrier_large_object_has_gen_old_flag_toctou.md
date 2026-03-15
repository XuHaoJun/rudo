# [Bug]: gc_cell_validate_and_barrier 大型物件路徑 has_gen_old_flag 讀取在 is_allocated 檢查之前 - TOCTOU

**Status:** Invalid
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發環境：lazy sweep 與 mutator 同時執行 |
| **Severity (嚴重程度)** | High | 可能導致 barrier 讀取已釋放物件的 flag，dirty tracking 混亂 |
| **Reproducibility (重現難度)** | High | 需要精確時序控制：slot sweep → reuse → flag read → is_allocated check |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc_cell_validate_and_barrier` large object path (heap.rs:2851-2857)
- **OS / Architecture:** All
- **Rust Version:** 1.75.0+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
應該先檢查 slot 是否仍然被分配 (`is_allocated()`)，然後再從該 slot 讀取任何 flag。

### 實際行為
在 `gc_cell_validate_and_barrier` 函數的**大型物件路徑**中：
1. **Line 2854**: 從 slot 讀取 `has_gen_old_flag()`
2. **Line 2876**: 返回 tuple（沒有大型物件的 is_allocated 檢查）

這導致在 lazy sweep 回收並重用 slot 後，barrier 可能讀取到已釋放物件的 stale flag。

**注意**：此 bug 與 bug278 不同。Bug278 涵蓋 regular object 路徑，此 bug 涵蓋 large object 路徑。

---

## 🔬 根本原因分析 (Root Cause Analysis)

問題位於 `crates/rudo-gc/src/heap.rs` 的 `gc_cell_validate_and_barrier` 函數的大型物件路徑：

```rust
// Line 2851-2857: 大型物件路徑 - 先讀取 flag，沒有 is_allocated 檢查
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;
// Skip barrier only if page is young AND object has no gen_old_flag (bug71).
// Cache flag to avoid TOCTOU between check and barrier (bug114).
let has_gen_old = (*gc_box_addr).has_gen_old_flag();  // <-- 先讀取 flag
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
// ... 沒有 is_allocated 檢查 ...
let owner = (*h_ptr).owner_thread;
// ...
(NonNull::new_unchecked(h_ptr), 0_usize)  // Line 2876
```

對比：regular object 路徑（lines 2883-2930）有 is_allocated 檢查：
- Line 2883: 讀取 flag
- Line 2893: 檢查 is_allocated
- Line 2899: 再次檢查 is_allocated

但 large object 路徑完全沒有 is_allocated 檢查！

並發場景：
1. Mutator A 正在執行 write barrier，pointer 指向大型物件的 slot
2. Mutator A 從 slot 讀取 `has_gen_old_flag()` (此時 slot 包含物件 A)
3. GC 執行 lazy sweep，回收物件 A 並將 slot 分配給新物件 B
4. Mutator A 執行 barrier，使用從舊物件 A 讀取的 flag 值

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 創建大型 GC 物件（使用 GcCell）
2. 在多個執行緒中並發修改大型 GcCell
3. 同時觸發 GC 進行 lazy sweep
4. 觀察 barrier 行為是否異常

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在大型物件路徑中添加 is_allocated 檢查：

```rust
let gc_box_addr = (head_addr + h_size) as *const GcBox<()>;

// 添加：先檢查 is_allocated
if !(*h_ptr).is_allocated(0) {
    return;
}

// 後讀取 flag
let has_gen_old = (*gc_box_addr).has_gen_old_flag();
if (*h_ptr).generation == 0 && !has_gen_old {
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
讀取已釋放物件的 flag 會導致 barrier 做出錯誤的決定。這可能導致 OLD→YOUNG 引用被錯誤地忽略，導致 young 物件被錯誤回收。

**Rustacean (Soundness 觀點):**
這是經典的 TOCTOU 漏洞。雖然不會直接導致 UAF，但會導致不一致的 barrier 行為，可能導致記憶體錯誤。

**Geohot (Exploit 攻擊觀點):**
攻擊者可能利用這個時序漏洞來控制 barrier 行為，進一步利用記憶體佈局進行攻擊。

---

## 🔗 相關 Issue

- bug278: `gc_cell_validate_and_barrier` regular object 路徑的相同問題
- bug286: barrier 函數 gen_old_flag 缺少 is_allocated 檢查

---

## Resolution (2026-03-15)

**Classified as Invalid.** The fix was already applied (bug247). The large object path in `gc_cell_validate_and_barrier` (heap.rs:2869-2876) now correctly checks `is_allocated(0)` **before** reading `has_gen_old_flag()`. The comment at line 2869 explicitly documents this: "Skip if slot was swept; read has_gen_old_flag only after is_allocated (bug247)." No code changes needed.
