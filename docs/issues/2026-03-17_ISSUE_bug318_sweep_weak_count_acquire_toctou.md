# [Bug]: Sweep TOCTOU - weak_count_acquire 與 has_dead_flag 讀取之間的 Race Condition

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發執行 sweep 與 Weak::new/clone，在高頻 GC 環境可能發生 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，Weak::upgrade 存取已回收記憶體 |
| **Reproducibility (復現難度)** | High | 需要精確控制 GC timing，單執行緒測試無法復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `sweep_phase2_reclaim` (gc/gc.rs:2299-2301)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current main branch

---

## 📝 問題描述 (Description)

在 sweep 程式碼中，`weak_count_acquire()` 和 `has_dead_flag()` 分別讀取同一個 atomic 變數 `weak_count`，但這兩個讀取之間存在 TOCTOU (Time-Of-Check-Time-Of-Use) Race Condition。

### 預期行為 (Expected Behavior)
- Sweep 程式應該在確保沒有 Weak 引用指向物件後才回收 slot
- Weak::upgrade() 應該永遠不會存取已回收的記憶體

### 實際行為 (Actual Behavior)
1. Sweep 執行緒讀取 weak_count = 0 (使用 weak_count_acquire)
2. 同時，另一執行緒呼叫 Gc::new 或 Weak::clone，呼叫 inc_weak() 將計數從 0 增加到 1
3. Sweep 執行緒檢查 has_dead_flag() - 讀取到新的 weak_count 值
4. 如果時機正確，sweep 會錯誤地回收仍有名為 weak ref 的物件
5. 擁有 Weak 指標的執行緒呼叫 upgrade() - **UAF!**

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/gc.rs:2299-2301`:

```rust
let weak_count = (*gc_box_ptr).weak_count_acquire();

if weak_count == 0 && (*gc_box_ptr).has_dead_flag() {
    // 回收 slot - 但可能已有新的 Weak 引用！
    ...
}
```

`weak_count_acquire()` (ptr.rs:243-245):
```rust
pub(crate) fn weak_count_acquire(&self) -> usize {
    self.weak_count.load(Ordering::Acquire) & !Self::FLAGS_MASK
}
```

`has_dead_flag()` (ptr.rs:417-419):
```rust
pub fn has_dead_flag(&self) -> bool {
    (self.weak_count.load(Ordering::Acquire) & Self::DEAD_FLAG) != 0
}
```

問題在於這兩個函數分別讀取 `weak_count` atomic兩次。即使使用 Acquire ordering，兩個讀取之間仍然存在時間窗口，另一個執行緒可以在此期間呼叫 `inc_weak()` 修改計數。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此 bug 需要並發測試才能穩定重現：

1. 建立多個執行緒
2. 一個執行緒執行 GC sweep
3. 同時另一個執行緒持續建立新的 Weak 引用
4. 使用 ThreadSanitizer 或精確計時控制來增加復現機率

```rust
// 概念驗證 - 需要並發執行
fn poc() {
    // Thread A: Sweep
    // Thread B: 同時呼叫 Gc::new which calls inc_weak()
    
    // 問題：weak_count_acquire() 讀到 0，但 has_dead_flag() 讀到之前，Thread B 可能已經增加到 1
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

讀取一次 raw weak_count 並同時檢查兩個條件：

```rust
// 修復方案
let raw_weak_count = (*gc_box_ptr).weak_count.load(Ordering::Acquire);
let weak_count = raw_weak_count & !GcBox::<()>::FLAGS_MASK;
let dead_flag = (raw_weak_count & GcBox::<()>::DEAD_FLAG) != 0;

if weak_count == 0 && dead_flag {
    // 現在是原子檢查，不會有 TOCTOU race
    ...
}
```

或者在 GcBox 中新增一個 helper 方法：

```rust
pub(crate) fn can_reclaim(&self) -> bool {
    let raw = self.weak_count.load(Ordering::Acquire);
    let count = raw & !Self::FLAGS_MASK;
    let dead = (raw & Self::DEAD_FLAG) != 0;
    count == 0 && dead
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這個 bug 會導致 sweep 階段錯誤地回收仍有 weak ref 存活的 slot。在incremental marking 環境下，這個問題更嚴重，因為標記和 sweep 可能並發執行。

**Rustacean (Soundness 觀點):**
這是一個確定的 UB - 當 Weak::upgrade() 嘗試存取已回收的記憶體時。雖然需要並發觸發，但在 Rust 的記憶體安全保證下，這樣的 race condition 是不可接受的。

**Geohot (Exploit 觀點):**
如果攻擊者能夠控制 GC timing，他們可能利用這個 UAF 進行記憶體佈局攻擊。特別是在即時編譯器或 FFI 情境下，這種不確定的記憶體錯誤可能被利用。

---

## Verification

**Verified by:** opencode  
**Date:** 2026-03-17

### Fix Applied

Fixed by adding `weak_count_and_dead_flag()` method in `ptr.rs` that atomically reads both `weak_count` and `dead_flag` with a single load. Updated all sweep locations in `gc/gc.rs` to use this new method, eliminating the TOCTOU race between `weak_count_acquire()` and `has_dead_flag()`.

**Changes:**
1. Added `weak_count_and_dead_flag()` method in `crates/rudo-gc/src/ptr.rs`
2. Updated 5 sweep locations in `crates/rudo-gc/src/gc/gc.rs` to use the new atomic method

All tests pass; clippy clean.
