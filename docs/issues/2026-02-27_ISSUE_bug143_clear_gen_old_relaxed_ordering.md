# [Bug]: GcBox::clear_gen_old 使用 Relaxed Ordering 導致潛在 Race Condition

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Low | 需要高並發場景：sweep 執行 clear_gen_old 的同時有 write barrier 讀取 |
| **Severity (嚴重程度)** | Medium | 可能導致錯誤的 barrier 行為，但不會造成 memory safety 問題 |
| **Reproducibility (復現難度)** | High | 需要精確時序控制才能穩定復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::clear_gen_old()` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

`GcBox::clear_gen_old()` 函數在清除 `GEN_OLD_FLAG` 時使用 `Ordering::Relaxed`，而 `has_gen_old_flag()` 使用 `Ordering::Acquire` 讀取。這種不一致的 ordering 可能在高並發場景下導致 race condition。

### 預期行為 (Expected Behavior)

當物件被 sweep 回收並釋放 slot 時，`GEN_OLD_FLAG` 應該被清除，且清除操作應該對並發的 write barrier 可見。

### 實際行為 (Actual Behavior)

`clear_gen_old()` 使用 `Ordering::Relaxed`:
```rust
// ptr.rs:363-368
pub(crate) fn clear_gen_old(&self) {
    self.weak_count
        .fetch_and(!Self::GEN_OLD_FLAG, Ordering::Relaxed);  // BUG: 應該使用更強的 ordering
}
```

而 `has_gen_old_flag()` 使用 `Ordering::Acquire`:
```rust
// ptr.rs:356-361
pub(crate) fn has_gen_old_flag(&self) -> bool {
    (self.weak_count.load(Ordering::Acquire) & Self::GEN_OLD_FLAG) != 0
}
```

這種不對稱的 ordering 導致：
1. Thread A: sweep 執行 `clear_gen_old()` 使用 Relaxed ordering
2. Thread B: write barrier 讀取 `has_gen_old_flag()` 使用 Acquire ordering
3. Thread B 可能仍然看到舊值 (GEN_OLD_FLAG = 1)，儘管 Thread A 已經清除

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:363-368`:
```rust
pub(crate) fn clear_gen_old(&self) {
    self.weak_count
        .fetch_and(!Self::GEN_OLD_FLAG, Ordering::Relaxed);  // <-- 問題所在
}
```

問題：
1. `Ordering::Relaxed` 只保證原子性，不保證跨執行緒的可見性
2. 配對的 `has_gen_old_flag()` 使用 `Ordering::Acquire`
3. 這種不對稱可能導致 write barrier 讀取到過時的 GEN_OLD_FLAG 值

影響：
- 在 sweep 清除 flag 的同時，如果 write barrier 讀取
- barrier 可能認為物件仍然是 OLD (有 GEN_OLD_FLAG)
- 這會導致額外的 barrier 工作，但不會造成正確性問題

注意：這與 bug25 有關聯，但 bug25 是關於 write barrier 讀取端使用 Relaxed ordering 的問題。本 bug 是關於清除端使用 Relaxed ordering 的互補問題。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要 ThreadSanitizer 才能穩定復現：

```rust
// 需要 TSan 才能穩定復現
fn test_clear_gen_old_race() {
    use std::thread;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    
    // 創建 GC 物件並觸發 OLD generation
    let gc = Gc::new(Data { value: 42 });
    // ... 觸發多次 GC 使其進入 OLD generation
    
    let barrier = Arc::new(AtomicBool::new(false));
    let ready = Arc::new(AtomicBool::new(false));
    
    let handles: Vec<_> = (0..4).map(|i| {
        let barrier = barrier.clone();
        let ready = ready.clone();
        thread::spawn(move || {
            if i == 0 {
                // Thread 0: 觸發 GC + sweep，執行 clear_gen_old
                // 這會清除 GEN_OLD_FLAG
                collect_full();  // 觸發 full GC
            } else {
                // 其他執行緒: 執行 write barrier，讀取 has_gen_old_flag
                // 使用 Acquire ordering
                while !ready.load(Ordering::SeqCst) {}
                let _ = cell.borrow_mut();  // 觸發 write barrier
            }
            barrier.store(true, Ordering::SeqCst);
        })
    }).collect();
    
    for h in handles {
        h.join().unwrap();
    }
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

將 `clear_gen_old()` 的 ordering 改為 `AcqRel` 或 `Release`，使其與 `has_gen_old_flag()` 的 `Acquire` 配對：

```rust
pub(crate) fn clear_gen_old(&self) {
    self.weak_count
        .fetch_and(!Self::GEN_OLD_FLAG, Ordering::AcqRel);  // 使用 AcqRel 確保清除被及時看到
}
```

或者使用 `Release`:
```rust
pub(crate) fn clear_gen_old(&self) {
    self.weak_count
        .fetch_and(!Self::GEN_OLD_FLAG, Ordering::Release);  // Release 確保寫入被後續 Acquire 讀取看到
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是 bug25 的互補問題。Bug25 修復了 write barrier 讀取端使用更強的 Acquire ordering，但清除端仍然使用 Relaxed。雖然這不會導致嚴重的正確性問題（只會導致額外的 barrier 工作），但為了完整性，應該對稱地使用適當的 ordering。

**Rustacean (Soundness 觀點):**
這不是 soundness 問題（使用 Relaxed 仍然是內存安全的），但可能導致不正確的 barrier 行為。在高並發場景下，這可能導致效能問題。

**Geohot (Exploit 攻擊觀點):**
在極端的時序控制下，攻擊者可能利用這個 race condition 來：
1. 強制執行額外的 barrier 工作（效能攻擊）
2. 尝试影响 GC 的行为时序

但實際利用難度很高，需要精確的時序控制。

---

## Resolution (2026-03-01)

**Outcome:** Already fixed.

`GcBox::clear_gen_old()` in `ptr.rs` (line 368) already uses `Ordering::Release`, not `Ordering::Relaxed` as the issue described. The Release/Acquire pairing between `clear_gen_old()` and `has_gen_old_flag()` is correct and documented in the inline comments (lines 363–364). No code changes needed.
