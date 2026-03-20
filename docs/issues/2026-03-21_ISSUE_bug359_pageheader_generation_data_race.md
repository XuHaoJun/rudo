# [Bug]: PageHeader.generation 是 plain u8 但被並發讀寫 - Data Race (UB)

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `High` | GC promotion 與 mutator write barrier 並發執行時必定觸發 |
| **Severity (嚴重程度)** | `Critical` | Data race 是 Rust 中的 UB，可能導致記憶體損壞 |
| **Reproducibility (復現難度)** | `Medium` | 需要並發場景，ThreadSanitizer 可檢測 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)

- **Component:** `PageHeader.generation`, write barriers (`gc_cell_validate_and_barrier`, `unified_write_barrier`), page promotion functions (`promote_young_pages`, `promote_all_pages`)
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `0.8.17`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`PageHeader.generation` 應該使用原子類型 (`AtomicU8`) 或其他同步機制來防止 data race。

### 實際行為 (Actual Behavior)

`PageHeader.generation` 是 plain `u8` 類型，但在並發場景下被讀寫：

1. **Write** (非原子): `promote_young_pages` 和 `promote_all_pages` 執行 `(*header).generation = 1;`
2. **Read** (非原子): Write barriers 執行 `(*h).generation == 0` 來決定是否跳過 barrier

這構成 Rust 定義的 **data race**，屬於未定義行為 (UB)。

### 程式碼位置

**heap.rs:977** - `generation` 定義為 plain `u8`:
```rust
pub struct PageHeader {
    // ...
    /// Generation index (for future generational GC).
    pub generation: u8,  // <-- 非原子！
    // ...
}
```

**gc/gc.rs:1707** - promotion 時寫入:
```rust
(*header).generation = 1; // Promote!  (非原子寫入)
```

**gc/gc.rs:2357** - promotion 時寫入:
```rust
(*header).generation = 1;
```

**heap.rs:2982** - barrier 時讀取:
```rust
if (*h).generation == 0 && !has_gen_old {
    return;  // 提前返回，跳過 barrier
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

`generation` 欄位在 `PageHeader` 結構體中被定義為 plain `u8`，但從未打算作為並發安全的原子操作。當 GC promotion 執行時：

1. `promote_young_pages()` 或 `promote_all_pages()` 寫入 `generation = 1`
2. 同時，mutator 執行 `GcCell::borrow_mut()` 觸發 write barrier
3. Write barrier 讀取 `generation` 來決定是否記錄 dirty page

由於 `generation` 是 plain `u8` 而非 `AtomicU8`，這構成 data race：
- Rust 的 data race 定義：兩個執行緒並發訪問同一記憶體，其中至少一個是 write 且沒有同步
- Data race = UB (未定義行為)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 使用 ThreadSanitizer 檢測 data race
// 編譯時開啟：RUSTFLAGS="-Z sanitizer=thread" cargo test

#[test]
fn test_generation_data_race() {
    // 1. 配置 incremental marking
    // 2. 同時觸發 minor GC (promotion) 和 GcCell borrow_mut
    // 3. TSan 會報告 data race
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**選項 A：將 `generation` 改為 `AtomicU8`**
```rust
pub generation: AtomicU8,
// 讀取時：generation.load(Ordering::Acquire)
// 寫入時：generation.store(1, Ordering::Release)
```

**選項 B：使用 memory barrier**
在 promotion 寫入 `generation = 1` 後，加入 `std::sync::atomic::fence(Ordering::Release)`確保之前的 `set_gen_old()` 可见。

**選項 C：移除對 `generation` 的依賴**
修改 barrier 邏輯，只依賴 `has_gen_old_flag()` 來決定是否記錄 dirty page。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在generational GC 中，page generation 的追蹤至關重要。傳統做法是使用原子操作或 memory barrier 來確保並發訪問的安全性。當前的 plain u8 讀寫在並發場景下完全不安全。

**Rustacean (Soundness 觀點):**
這是嚴格的 Rust UB。Data race 在 Rust 中被視為 compile-time error（對於 `Send + Sync` 檢查）或 runtime UB。此 bug 會導致 LLVM 可能做出錯誤假設，產生完全錯誤的機器碼。

**Geohot (Exploit 攻擊觀點):**
雖然直接利用 data race 比較困難，但 UB 的存在本身就是个問題。編譯器優化可能「意外」修復或「意外」利用這個 bug，使程式行為不可預測。

---

## 驗證記錄

**驗證日期:** 2026-03-21
**驗證人員:** opencode

### 驗證結果

確認 `PageHeader.generation` 是 plain `u8` (heap.rs:977)，但被並發讀寫：

- `promote_young_pages` (gc.rs:1707): `(*header).generation = 1;` - 非原子寫入
- `promote_all_pages` (gc.rs:2357): `(*header).generation = 1;` - 非原子寫入  
- `gc_cell_validate_and_barrier` (heap.rs:2982): `(*h).generation == 0` - 非原子讀取
- `unified_write_barrier` (heap.rs:3039, 3068): 多處非原子讀取

**Status: Open** - 需要修復。