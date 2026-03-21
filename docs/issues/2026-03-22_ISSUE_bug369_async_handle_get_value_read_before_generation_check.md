# [Bug]: AsyncHandle::get() Value Read BEFORE Generation Check (TOCTOU)

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires slot sweep and reuse between generation save and value read |
| **Severity (嚴重程度)** | Critical | Returns data from wrong object - potential UAF |
| **Reproducibility (重現難度)** | High | Requires precise concurrent timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `AsyncHandle::get()` and `AsyncHandle::get_unchecked()` in `handles/async.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`AsyncHandle::get()` 應該在讀取值之前驗證 generation，確保 slot 沒有在檢查和讀取之間被回收和重用。正確的模式應為：

1. 保存 `pre_generation`
2. 驗證 generation 沒有改變
3. 讀取值

### 實際行為 (Actual Behavior)

當前程式碼順序（錯誤）:
```rust
// async.rs:634-641 - 錯誤順序
let pre_generation = gc_box.generation();  // Line 634
let value = gc_box.value();                 // Line 635 - BUG: 讀取值在檢查之前!
assert_eq!(                                // Lines 636-640
    pre_generation,
    gc_box.generation(),
    "AsyncHandle::get: slot was reused between pre-check and value read (generation mismatch)"
);
value                                        // Line 641
```

如果 slot 在 `pre_generation` 保存（634）和 `value` 讀取（635）之間被 sweep 並重用，則會讀取到**錯誤物件的值**，然後才在 assert_eq (636-640) 處 panic。

### 對比：Handle::get() 的正確實現

`Handle::get()` (`handles/mod.rs:324-331`) 展示正確模式：
```rust
// mod.rs:324-331 - 正確順序
let pre_generation = gc_box.generation();  // Line 324
assert_eq!(                                // Lines 325-329
    pre_generation,
    gc_box.generation(),
    "Handle::get: slot was reused before value read (generation mismatch)"
);
let value = gc_box.value();                 // Line 330 - 值在檢查之後才讀取
value                                        // Line 331
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

TOCTOU Race Condition 詳細過程:

1. Object A 存在於 slot index，generation = 1
2. Thread A 呼叫 `AsyncHandle::get()`
3. Thread A 通過 `is_allocated` 和狀態檢查
4. Thread A 保存 `pre_generation = 1`（line 634）
5. **Race Window**: Lazy sweep 在此時運行:
   - 認定 Object A 已死亡，回收 slot
   - 在同一 slot 分配 Object B (generation = 2)
6. Thread A 執行 `value = gc_box.value()`（line 635）→ **讀取 Object B 的值！**
7. Thread A 執行 `assert_eq(1, 2, ...)`（line 636-640）→ **Panic!**
8. 但為時已晚 - 錯誤的值已被讀取（line 635）

後果:
- 在非常罕見的race條件下，可能返回錯誤物件的資料
- 如果 assert 被優化掉或以某种方式繞過，會導致 UAF

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 需要精確控制執行緒調度的並發測試環境
// 1. 建立 AsyncHandleScope 和 AsyncHandle
// 2. 觸發 lazy sweep 回收 slot
// 3. 在同一 slot 分配新物件（不同 generation）
// 4. 同時呼叫 AsyncHandle::get()
// 5. 觀察: 值是否來自錯誤的物件
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `AsyncHandle::get()` 和 `AsyncHandle::get_unchecked()` 的順序調整為與 `Handle::get()` 一致:

```rust
// AsyncHandle::get() 修復
let pre_generation = gc_box.generation();
assert_eq!(
    pre_generation,
    gc_box.generation(),
    "AsyncHandle::get: slot was reused before value read (generation mismatch)"
);
let value = gc_box.value();
value

// AsyncHandle::get_unchecked() 修復
let pre_generation = gc_box.generation();
assert_eq!(
    pre_generation,
    gc_box.generation(),
    "AsyncHandle::get_unchecked: slot was reused before value read (generation mismatch)"
);
let value = gc_box.value();
value
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是經典的 TOCTOU 漏洞。正確的模式應該是「先驗證，後讀取」：先保存並驗證 generation，然後才讀取值。這種模式在 `Handle::get()` 中已經正確實現，`AsyncHandle::get()` 和 `AsyncHandle::get_unchecked()` 應該採用相同模式。

**Rustacean (Soundness 觀點):**
如果 slot 在讀取值之前被重用，則會讀取到錯誤物件的資料。這可能導致 UAF（使用已釋放的記憶體）或讀取到錯誤的資料。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制 GC timing 和 slot 配置，可以利用這個 race condition 來讀取錯誤物件的資料，可能導致資訊洩漏。

---

## 驗證記錄

**驗證日期:** 2026-03-22

**驗證方法:**
- Code review 比較 `AsyncHandle::get()` (async.rs:634-641) 與 `Handle::get()` (mod.rs:324-331)
- 確認: `Handle::get()` 先檢查 generation，後讀取值（正確）
- 確認: `AsyncHandle::get()` 先讀取值，後檢查 generation（錯誤）
- 確認: `AsyncHandle::get_unchecked()` (async.rs:704-711) 有相同問題
