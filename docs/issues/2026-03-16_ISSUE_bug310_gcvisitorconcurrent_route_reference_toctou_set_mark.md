# [Bug]: GcVisitorConcurrent::route_reference TOCTOU - set_mark return value ignored

**Status:** Verified
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並髮執行 lazy sweep 與 incremental/parallel marking |
| **Severity (嚴重程度)** | High | 導致記憶體洩漏 - 新配置的物件被錯誤標記為存活 |
| **Reproducibility (復現難度)** | High | 需要精確控制 lazy sweep 與 marking 的執行時序 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcVisitorConcurrent::route_reference` in `trace.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`GcVisitorConcurrent::route_reference` 函數存在 TOCTOU (Time-Of-Check-Time-Of-Use) 漏洞。在檢查 `is_allocated` 之後、呼叫 `set_mark` 之前，lazy sweep 可能已經釋放並重新分配該 slot。

### 預期行為
在標記物件前，應該確保 slot 仍然被分配，且標記操作成功。

### 實際行為
`is_allocated` 檢查通過後，`set_mark` 的回傳值被完全忽略。如果 slot 在檢查後被 sweep 並重新分配，新物件會被錯誤地標記為存活。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/trace.rs:179-185`:

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    return;
}
if (*header.as_ptr()).is_marked(idx) {
    return;
}
(*header.as_ptr()).set_mark(idx);  // 回傳值被忽略!
```

**問題:**
1. Line 179-181: 檢查 `is_allocated` - 這是 bug129 修復後的結果
2. Line 185: 呼叫 `set_mark` 但忽略回傳值
3. **TOCTOU 視窗**: 檢查和標記之間，lazy sweep 可以:
   - Sweep 該 slot (設 `is_allocated` 為 false)
   - 重新分配給新物件
   - 標記被錯誤地應用於新物件!

**對比正確實作** (`mark_and_trace_incremental` in `gc/gc.rs:2400-2410`):
```rust
loop {
    match (*header.as_ptr()).try_mark(idx) {
        Ok(true) => {
            // 標記成功後重新檢查 is_allocated!
            if !(*header.as_ptr()).is_allocated(idx) {
                (*header.as_ptr()).clear_mark_atomic(idx);
                return;
            }
            visitor.objects_marked += 1;
            break;
        }
        // ...
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並髮執行:
1. 執行緒 A: 正在進行 incremental/parallel marking
2. 執行緒 B: 同時執行 lazy sweep

精確時序:
- `route_reference` 檢查 `is_allocated` → true
- Lazy sweep sweep 該 slot 並重新分配
- `set_mark` 被呼叫，錯誤地標記新物件

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

使用 `try_mark` + 事後檢查模式:

```rust
loop {
    match (*header.as_ptr()).try_mark(idx) {
        Ok(true) => {
            // 標記成功後重新檢查 is_allocated!
            if !(*header.as_ptr()).is_allocated(idx) {
                (*header.as_ptr()).clear_mark_atomic(idx);
                return;
            }
            break;
        }
        Ok(false) => return, // 已經標記過
        Err(_) => continue, // CAS 失敗，重試
    }
}
```

或至少檢查 `set_mark` 回傳值並重新驗證:

```rust
if !(*header.as_ptr()).set_mark(idx) {
    return; // 已經標記
}
// 重新檢查 is_allocated - 如果被 sweep 就清除標記
if !(*header.as_ptr()).is_allocated(idx) {
    (*header.as_ptr()).clear_mark_atomic(idx);
    return;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
在 SATB (Snapshot-At-The-Beginning) GC 中，所有在 snapshot 時存活的物件都必須被正確標記。這個 TOCTOU 漏洞會導致不正確的標記，違反 SATB 的正確性。新物件被錯誤標記會導致記憶體洩漏 - 該物件及其可達的所有物件都無法被回收。

**Rustacean (Soundness 觀點):**
這不是傳統意義上的 UB，但可能導致記憶體腐敗（錯誤的標記狀態）。`set_mark` 的回傳值存在但被忽略，這表明開發者可能沒有意識到這個 race 條件。

**Geohot (Exploit 觀點):**
攻擊者可以嘗試控制 lazy sweep 的時序來觸發這個 bug。雖然難以可靠觸發，但成功利用可導致記憶體洩漏（拒絕服務）。在即時編譯器 (JIT) 場景中，控制記憶體配置時機可能更容易觸發。

---

## Verification

**Verified by:** opencode  
**Date:** 2026-03-17

### Code Location Confirmed

The bug is confirmed at `crates/rudo-gc/src/trace.rs:179-185`:

```rust
if !(*header.as_ptr()).is_allocated(idx) {
    return;
}
if (*header.as_ptr()).is_marked(idx) {
    return;
}
(*header.as_ptr()).set_mark(idx);  // Return value IGNORED!
```

**Issue confirmed:**
- Line 185 calls `set_mark(idx)` but ignores the return value
- Between the `is_allocated` check (line 179) and `set_mark` (line 185), lazy sweep can:
  1. Sweep the slot (set `is_allocated` to false)
  2. Reallocate to a new object
  3. The mark gets incorrectly applied to the new object

**Contrast with correct implementation** in `gc/gc.rs:2400-2410` which uses `try_mark` + post-CAS validation.

**Status: VERIFIED - Bug confirmed present in code**
