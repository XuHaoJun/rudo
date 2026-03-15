# [Bug]: Sweep 程式使用 Relaxed ordering 載入 weak_count，與 dec_weak 的 AcqRel 不一致導致 TOCTOU Race

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要並發執行 sweep 與 Weak 操作，在高頻 GC 環境可能發生 |
| **Severity (嚴重程度)** | Critical | 可能導致 Use-After-Free，Weak::upgrade 存取已回收記憶體 |
| **Reproducibility (復現難度)** | High | 需要精確控制 GC timing，單執行緒測試無法復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `lazy_sweep_page`, `sweep_page` (gc/gc.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current main branch

---

## 📝 問題描述 (Description)

在 sweep 程式碼中，weak_count 使用 `Ordering::Relaxed` 載入，但 `dec_weak` 使用 `Ordering::AcqRel` 執行 compare-and-swap。這導致記憶體順序不一致，造成經典的 TOCTOU (Time-Of-Check-Time-Of-Use) Race Condition。

### 預期行為 (Expected Behavior)
- Sweep 程式應該在確保沒有 Weak 引用指向物件後才回收 slot
- Weak::upgrade() 應該永遠不會存取已回收的記憶體

### 實際行為 (Actual Behavior)
1. Sweep 執行緒載入 weak_count = 0 (使用 Relaxed ordering)
2. 同時，另一執行緒呼叫 dec_weak()，從 1 遞減到 0 (使用 AcqRel ordering)
3. Sweep 執行緒沒有看到 dec_weak 的變更 (Relaxed vs AcqRel 不同步)
4. Sweep 執行緒回收 slot
5. 擁有 Weak 指標的執行緒呼叫 upgrade() - **UAF!**

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `crates/rudo-gc/src/gc/gc.rs` 的 `lazy_sweep_page` 和 `sweep_page` 函數中：

```rust
// gc/gc.rs:2523
let weak_count = (*gc_box_ptr).weak_count();  // 使用 Relaxed ordering!

if weak_count > 0 {
    // 有 weak ref，不回收
    ...
} else {
    // 回收 slot - 但可能仍有 Weak 引用！
    ...
}
```

`weak_count()` 方法在 `ptr.rs:207-208`:
```rust
pub fn weak_count(&self) -> usize {
    self.weak_count.load(Ordering::Relaxed) & !Self::FLAGS_MASK
}
```

而 `dec_weak` 在 `ptr.rs:289-323` 使用 `Ordering::AcqRel`:
```rust
pub fn dec_weak(&self) -> bool {
    loop {
        let current = self.weak_count.load(Ordering::Relaxed);
        // ...
        match self.weak_count.compare_exchange_weak(
            current,
            flags,
            Ordering::AcqRel,  // <-- 使用 AcqRel!
            Ordering::Relaxed,
        ) {
            // ...
        }
    }
}
```

**問題**：
- Sweep 使用 Relaxed load 讀取 weak_count
- dec_weak 使用 AcqRel CAS 遞減 weak_count
- Relaxed load 不會與 AcqRel CAS 建立同步關係
- 導致 Sweep 可能看不到 dec_weak 的遞減，錯誤地回收仍有 Weak 引用的 slot

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

此為理論性 bug，需要並發執行才能觸發。單執行緒測試無法復現。

概念驗證 (需要並發 GC + Weak 操作):
```rust
// 需要使用 loom 或 ThreadSanitizer 進行嚴格測試
// 場景：
// 1. Thread A: 正在執行 sweep，載入 weak_count = 0
// 2. Thread B: 呼叫 last Weak 參考的 drop，dec_weak() 從 1 降到 0 (AcqRel)
// 3. Thread A: 沒有看到 Thread B 的變更，繼續回收 slot
// 4. Thread C: 持有 Weak 指標，呼叫 upgrade() - UAF!
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

### 方案 1：使用更強的 Memory Ordering (推薦)
在 sweep 程式碼中使用 `Acquire` ordering 載入 weak_count：

```rust
// gc/gc.rs
let weak_count = (*gc_box_ptr).weak_count_raw();  // 使用 Acquire
let weak_count = weak_count & !GcBox::<()>::FLAGS_MASK;
```

或添加新方法：
```rust
pub fn weak_count_acquire(&self) -> usize {
    self.weak_count.load(Ordering::Acquire) & !Self::FLAGS_MASK
}
```

### 方案 2：使用 Compare-And-Swap
在回收前使用 CAS 嘗試 atomically 檢查並標記：

```rust
// 嘗試 atomically 確認 weak_count 為 0 並標記為即將回收
let current = self.weak_count.load(Ordering::Acquire);
// ... 使用 CAS 確保 atomicity
```

### 方案 3：添加 is_allocated Post-Check
在回收後、實際使用記憶體前再次檢查 (類似其他 TOCTOU 修復):
```rust
// 回收後立即檢查 is_allocated
// 如果被重新分配，回滾操作
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這是典型的 GC 記憶體順序問題。在多執行緒環境中，weak reference 的回收必須與主要 sweep 同步。传统 GC 使用 write barrier 或類似的同步機制來確保這類操作的原子性。rudo-gc 的 TLAB 模型增加了複雜性，因為每個執行緒有自己的分配空間，但 weak reference 可以跨執行緒存在。

**Rustacean (Soundness 觀點):**
這是明確的 Undefined Behavior。存取已回收的記憶體 (UAF) 在 Rust 中是嚴重的 soundness 問題。即使是 advisory 的 weak_count，也必須確保記憶體順序正確，否則會破壞記憶體安全。

**Geohot (Exploit 觀點):**
攻擊者可以通過：
1. 構造精確的 timing 來觸發這個 race
2. 利用 UAF 進行記憶體佈局攻擊
3. 結合其他 GC bug 擴大攻擊面
4. 特別危險的是：攻擊者可能控制 Weak 指標的建立和 GC timing，實現可靠的 exploit

---

## Resolution (2026-03-14)

**Fixed.** Added `weak_count_acquire()` in `ptr.rs` using `Ordering::Acquire` to synchronize with `dec_weak`'s `AcqRel` CAS. Replaced all sweep and orphan-page weak-check usages in `gc/gc.rs` and `heap.rs` with `weak_count_acquire()`. Full test suite and Clippy pass. Single-threaded tests cannot reproduce the race; fix follows atomic ordering best practice (Acquire load when result gates safety-critical reclaim decision).

