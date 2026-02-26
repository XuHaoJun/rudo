# [Bug]: stop_all_mutators_for_snapshot 會在多執行緒環境下無窮迴圈

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | High | 任何使用 incremental marking 的多執行緒程式都會觸發 |
| **Severity (Critical)** | Critical | 導致程式無窮迴圈，完全無回應 |
| **Reproducibility (復現難度)** | Medium | 需要多執行緒 + incremental marking 啟用 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** Incremental Marking - `stop_all_mutators_for_snapshot` handshake
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

`stop_all_mutators_for_snapshot` 函數中的 stop-the-world 握手協議存在錯誤，導致多執行緒程式發生無窮迴圈。

### 預期行為
GC 執行時應該等待所有 mutator 執行緒抵達 safepoint，然後繼續執行標記階段。

### 實際行為
當 `incremental marking` 啟用且有多個執行緒時，函數會無窮迴圈，永遠無法完成 snapshot 階段。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/incremental.rs:508-542` 的 `stop_all_mutators_for_snapshot` 函數中：

1. **握手協議錯誤**：
   - 函數只將 `rendezvous_ack_counter` 增加一次（line 519）
   - 當 mutator 執行緒抵達 safepoint 時，`enter_rendezvous()` (heap.rs:623-664) 只會遞減 `active_count`，但從未呼叫 `increment_rendezvous_ack()`

2. **迴圈條件缺陷**：
   ```rust
   if active == 1 && ack_count >= thread_count {
       break;
   }
   ```
   
3. **問題觸發**：
   - 假設有 2 個執行緒（1 個 GC + 1 個 mutator）
   - `thread_count = 2`
   - `ack_count = 1`（只在 GC 執行緒增加一次）
   - 當 mutator 抵達 safepoint，`active = 1`
   - 條件：`1 >= 2` = **FALSE**
   - 迴圈永遠無法 break → **無窮迴圈！**

4. **Race condition 加劇問題**：
   - 如果有新執行緒在初始 `thread_count` 讀取後、但在迴圈檢查前註冊，`thread_count` 會增加，使條件更難滿足

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// 啟用 incremental marking
use rudo_gc::{Gc, Trace, gc::incremental::IncrementalConfig, gc::incremental::set_incremental_config};
use std::thread;

#[derive(Trace)]
struct Data { value: i32 }

fn main() {
    // 啟用 incremental marking
    set_incremental_config(IncrementalConfig {
        enabled: true,
        ..Default::default()
    });

    let gc = Gc::new(Data { value: 42 });
    
    // 建立多個執行緒
    let handles: Vec<_> = (0..2).map(|_| {
        thread::spawn(move || {
            // 每個執行緒進行 GC 指標操作
            let _ = gc.clone();
        })
    }).collect();

    for h in handles {
        h.join().unwrap();
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix)

**選項 1：移除損壝的檢查**
由於 `active == 1` 已經足夠信號所有 mutator 已抵達 safepoint，移除 `ack_count >= thread_count` 檢查：

```rust
if active == 1 {
    break;
}
```

**選項 2：修復握手協議**
在 `enter_rendezvous()` 中當執行緒抵達 safepoint 時，增加 `rendezvous_ack_counter`：

```rust
// 在 heap.rs:enter_rendezvous() 的 active_count.fetch_sub之後
crate::gc::incremental::IncrementalMarkState::global()
    .increment_rendezvous_ack();
```

**建議採用選項 1**，因為 `active == 1` 已經是充分的條件。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Stop-the-world 握手協議需要確保所有 mutator 執行緒都已停止。`active_count == 1` 代表只有 GC 執行緒正在運行，這是正確的信號。`rendezvous_ack_counter` 的設計似乎是一個未完成的額外檢查，但從未被正確實現（mutator 執行緒從未遞增它）。

**Rustacean (Soundness 觀點):**
這不是一個記憶體安全問題，而是一個活性問題（liveness issue）。程式不會崩潰或產生錯誤的記憶體操作，而是無限期地迴圈。這是 GC 實現中的嚴重 bug，但不會導致 UB。

**Geohot (Exploit 觀點):**
這可以被利用作為一種拒絕服務（DoS）攻擊。如果攻擊者能夠控制執行緒數量或 GC 觸發，他們可以導致服務無限期掛起。不過這需要能夠觸發 GC，這在正常操作中可能是可接受的。

---

## Resolution (2026-02-27)

**Outcome:** Fixed.

Applied Option 1: removed the broken `ack_count >= thread_count` check. The `rendezvous_ack_counter` was never fully wired—mutator threads never call `increment_rendezvous_ack()` in `enter_rendezvous()`, so `ack_count` stayed at 1 (only the collector incremented it) while `thread_count` was ≥2, making the condition unreachable.

The condition `active == 1` is sufficient: when all mutators reach safepoint they decrement `active_count` in `enter_rendezvous()`, so `active == 1` means only the collector is running. The loop in `stop_all_mutators_for_snapshot` now breaks on `active == 1` only.
