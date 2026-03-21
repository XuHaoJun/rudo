# [Bug]: PageHeader generation 和 gen_old_flag 更新順序導致 TOCTOU 漏洞

**Status:** Invalid
**Tags:** Not Reproduced

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Medium` | 需要在 GC promotion 期間同時有 mutator 執行 write barrier |
| **Severity (嚴重程度)** | `High` | 導致 OLD→YOUNG 引用遺漏，年輕物件可能被錯誤回收 |
| **Reproducibility (復現難度)** | `High` | 需要精確的執行時序，單執行緒無法觸發 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `PageHeader.generation`, `GcBox::gen_old_flag`, write barrier, generational GC promotion
- **OS / Architecture:** `Linux x86_64`, `All`
- **Rust Version:** `1.75.0`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
在頁面 promotion 期間，`generation` 欄位更新為 1（表示 old generation）與 `gen_old_flag` 標記設置應該是原子的，或者至少在順序上確保 barrier 檢查時兩者一致。

### 實際行為 (Actual Behavior)
在 `promote_young_pages()` (gc.rs:1707) 和 `promote_all_pages()` (gc.rs:2357) 中：
1. 先以非原子方式寫入 `generation = 1`
2. 之後才以原子方式呼叫 `set_gen_old()` 設置 per-object flag

這創造了一個 TOCTOU 視窗，期間：
- `generation == 1` (old page)
- 但 `has_gen_old_flag()` 返回 `false` (flag 尚未設置)

導致 barrier 可能做出錯誤決定。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `gc/gc.rs` 的 `collect_minor` 函數中 (lines 1707-1722)：
```rust
(*header).generation = 1; // Promote!  (非原子寫入)

for word_idx in 0..crate::heap::BITMAP_SIZE {
    // ...
    (*gc_box_addr).set_gen_old();  // 延後設置 (原子操作)
}
```

同樣的問題也存在於 `promote_all_pages()` (lines 2357-2371)。

在 `heap.rs:gc_cell_validate_and_barrier` (lines 2980-2982)：
```rust
if (*h).generation == 0 && !has_gen_old {
    return;  // 提前返回，跳過 barrier
}
```

在 promotion 期間，會有短暫時間：
- GC 執行緒已寫入 `generation = 1`
- 但尚未執行 `set_gen_old()`

此時 mutator 執行 write barrier：
- 讀取 `generation == 1` (不是 0)
- 讀取 `has_gen_old == false`
- 由於 `generation != 0`，barrier 會記錄 dirty page (這是正確的)

但另一種情況：
- 若 `generation` 因 Bug122 尚未同步 (非原子讀取)
- 或在更細粒度的某處有 TOCTOU

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 啟動多個執行緒
2. 一個執行緒執行 minor GC (觸發 promotion)
3. 同時其他執行緒執行大量 GcCell::borrow_mut() 操作
4. 使用 ThreadSanitizer 或 Miri 檢測

```rust
// 概念驗證需要精確時序
// 此為理論分析，非可運行的 PoC
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

1. **選項 A：將 generation 改為 AtomicU8**
   - 參考 Bug122 的修復方式
   
2. **選項 B：在 promotion 時使用 memory barrier**
   - 使用 `std::sync::atomic::fence(Ordering::Release)` 在 `generation = 1` 之後
   - 確保 `set_gen_old()` 可見

3. **選項 C：改變檢查邏輯**
   - 只要 `generation > 0` 就記錄 dirty page
   - 不依賴 `gen_old_flag` 進行 early-exit

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 generational GC 中，page generation 和 per-object gen_old_flag 必須保持一致性。傳統做法是使用 card table 或 remembered set 來追蹤 OLD→YOUNG 引用。當前實現依賴雙重檢查（generation + flag），但在並發 promotion 期間會產生不一致窗口。

**Rustacean (Soundness 觀點):**
這不僅是 TOCTOU 效率問題，更是數據 race。`generation` 是非原子類型，但被並發讀寫 (Bug122)。即使忽略 Bug122，這裡的 ordering 問題也會導致 barrier 邏輯錯誤。

**Geohot (Exploit 觀點):**
理論上可利用此 TOCTOU 視窗：
- 預測 GC timing
- 在 promotion 期間觸發大量 OLD→YOUNG 寫入
- 導致 young object 被錯誤回收
- 製造 use-after-free 場景

但實際利用難度較高，需要精確控制時序。

---

## Resolution (2026-03-21)

**Outcome:** Invalid — no actionable bug in current code.

**Analysis:**

The issue's primary concern was that `PageHeader.generation` was a non-atomic plain `u8`,
causing data races during concurrent promotion + barrier execution. In the current codebase,
`generation` is declared as `AtomicU8` (`heap.rs:989`) with `Ordering::Release` stores
(`gc.rs:1712`, `gc.rs:2362`) and `Ordering::Acquire` loads in every barrier call site
(`heap.rs:2832`, `2861`, `2928`, `3001`, `3058`, `3087`). Similarly, `set_gen_old()` uses
`Release` and `has_gen_old_flag()` uses `Acquire` (`ptr.rs:483–492`).

The remaining TOCTOU window (generation=1 stored, `set_gen_old()` not yet called) does **not**
produce incorrect barrier behavior. The early-exit condition is:

```rust
if generation.load(Acquire) == 0 && !has_gen_old { return; }
```

During that window the barrier sees `generation == 1`, so the `== 0` arm is false and the
barrier fires correctly — the issue itself acknowledges this: "由於 generation != 0，barrier
會記錄 dirty page (這是正確的)". The secondary concern ("若 generation 因 Bug122 尚未同步")
referred to the non-atomic access, which is now fixed.

No code change required.
