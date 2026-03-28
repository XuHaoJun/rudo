# [Bug]: GcBox::set_dead uses Relaxed ordering but has_dead_flag uses Acquire - Synchronization Bug

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | High concurrency: set_dead (Relaxed) while another thread calls has_dead_flag (Acquire) |
| **Severity (嚴重程度)** | High | Data race - UB in Rust, could cause incorrect dead flag visibility |
| **Reproducibility (復現難度)** | High | Requires precise concurrent timing; TSan can detect |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcBox::set_dead()` (ptr.rs), `GcBox::has_dead_flag()` (ptr.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為
`set_dead` should use `Ordering::Release` to properly synchronize with `Acquire` readers like `has_dead_flag`.

### 實際行為
`set_dead` uses `Ordering::Relaxed` (ptr.rs:456), but `has_dead_flag` uses `Ordering::Acquire` (ptr.rs:437). This breaks the synchronization contract.

The comment at line 473-475 explicitly states the correct pattern:
> "Uses `Release` ordering for both operations so that concurrent threads doing `Acquire` loads (e.g. `has_dead_flag`, `is_under_construction`) correctly observe the updated state."

But `set_dead` uses `Relaxed` instead of `Release`.

---

## 🔬 根本原因分析 (Root Cause Analysis)

**檔案:** `crates/rudo-gc/src/ptr.rs`

**問題函數:**
```rust
// Line 455-456 - set_dead uses Relaxed:
pub(crate) fn set_dead(&self) {
    self.weak_count.fetch_or(Self::DEAD_FLAG, Ordering::Relaxed);  // <-- BUG
}

// Line 436-437 - has_dead_flag uses Acquire:
pub(crate) fn has_dead_flag(&self) -> bool {
    (self.weak_count.load(Ordering::Acquire) & Self::DEAD_FLAG) != 0  // expects Release writer
}
```

**同步契約被破壞:**
- `has_dead_flag` uses `Acquire` - expects a `Release` write to synchronize
- `set_dead` uses `Relaxed` - does NOT provide Release semantics
- If Thread A calls `set_dead` (Relaxed) and Thread B calls `has_dead_flag` (Acquire), Thread B may NOT see the DEAD_FLAG update

**bug145 修復不完整:**
- bug145 was marked Fixed
- `has_dead_flag` was changed from Relaxed to Acquire (correct)
- `set_dead` was NOT changed from Relaxed to Release (incorrect)

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Thread A: Writer (mutator)
gc_box.set_dead();  // Relaxed store

// Thread B: Reader (GC)
if gc_box.has_dead_flag() {  // Acquire load
    // Thread B may NOT see Thread A's update - data race!
}
```

**Note:** This is a data race (UB) and should be detected by ThreadSanitizer.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change `set_dead` to use `Ordering::Release`:

```rust
pub(crate) fn set_dead(&self) {
    self.weak_count.fetch_or(Self::DEAD_FLAG, Ordering::Release);  // Was Relaxed
}
```

This makes `set_dead` consistent with:
1. `mark_dead` which uses `Release` (and has explicit comment explaining why)
2. `has_dead_flag` which uses `Acquire` (expects Release writer)

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The DEAD_FLAG is a synchronization flag used between the mutator (setting dead) and GC (checking dead). Using Relaxed for set_dead and Acquire for has_dead_flag breaks the necessary happens-before relationship. This could cause the GC to miss dead objects during marking.

**Rustacean (Soundness 觀點):**
This is a data race - undefined behavior in Rust. The compiler/hardware can reorder or cache the Relaxed write, so a subsequent Acquire read may not see it. This is soundness-violating UB.

**Geohot (Exploit 攻擊觀點):**
If the DEAD_FLAG is not visible to the GC, an object could be incorrectly considered live. While hard to exploit directly, UB makes the program's behavior unpredictable.

---

## 🔗 相關 Issue

- bug145: clear_dead Relaxed ordering - marked Fixed (but partial fix)
- bug318: sweep weak_count Acquire TOCTOU - Fixed

---

## 驗證記錄

**驗證日期:** 2026-03-21

**驗證方法:**
1. Static code analysis: confirmed `set_dead` uses Relaxed (line 456)
2. Confirmed `has_dead_flag` uses Acquire (line 437)
3. Confirmed `mark_dead` uses Release (line 477) with explicit comment
4. This is a regression from bug145's incomplete fix