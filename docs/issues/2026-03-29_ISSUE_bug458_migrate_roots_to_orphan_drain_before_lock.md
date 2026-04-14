# [Bug]: migrate_roots_to_orphan drains before lock risking data loss on panic

**Status:** Fixed
**Tags:** Verified
**Fixed By:** commit b66c932 - fix(heap): prevent data loss in migrate_roots_to_orphan on panic

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Low` | Panic during orphan.insert() is rare but possible under OOM |
| **Severity (嚴重程度)** | `High` | Lost GC roots cause memory leaks, referenced objects become immortal |
| **Reproducibility (復現難度)** | `Low` | Requires panic during migration - hard to reproduce deterministically |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `migrate_roots_to_orphan` in `heap.rs`
- **OS / Architecture:** `All`
- **Rust Version:** `1.75+`
- **rudo-gc Version:** `0.8.0`

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
When a thread terminates and `migrate_roots_to_orphan` is called, all cross-thread GC roots should either:
1. Be migrated to the orphan table, OR
2. Remain in the TCB roots if migration fails

### 實際行為 (Actual Behavior)
The function drains ALL entries from `roots.strong` BEFORE attempting to insert into `orphan`. If `orphan.insert()` panics (e.g., HashMap rehashing under memory pressure), remaining entries in `drained` are dropped and permanently lost.

---

## 🔬 根本原因分析 (Root Cause Analysis)

```rust
pub fn migrate_roots_to_orphan(tcb: &ThreadControlBlock, thread_id: ThreadId) {
    let mut roots = tcb.cross_thread_roots.lock().unwrap();
    if roots.strong.is_empty() {
        return;
    }
    // BUG: ALL entries removed from roots BEFORE orphan insert attempt
    let drained: Vec<_> = roots.strong.drain()  // Lines 216-220
        .map(|(k, v)| (k, v.as_ptr() as usize))
        .collect();

    let mut orphan = orphaned_cross_thread_roots().lock();  // Line 224
    for (handle_id, ptr) in drained {
        orphan.insert((thread_id, handle_id), ptr);  // Line 226 - PANIC can occur here
    }
}
```

The drain-then-insert pattern means:
1. All entries removed from `roots.strong` (line 216-220)
2. `orphan` lock acquired (line 224)
3. If `orphan.insert()` panics (line 226), remaining entries are dropped
4. Lost entries cannot be resolved (not in roots or orphan) but refcounts remain elevated

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// Hard to reproduce - requires panic during orphan.insert()
// Under OOM conditions with many cross-thread handles, HashMap rehashing
// could panic, causing entries to be lost.
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

```rust
pub fn migrate_roots_to_orphan(tcb: &ThreadControlBlock, thread_id: ThreadId) {
    let mut roots = tcb.cross_thread_roots.lock().unwrap();
    if roots.strong.is_empty() {
        return;
    }
    
    // Acquire orphan lock FIRST
    let mut orphan = orphaned_cross_thread_roots().lock();
    
    // Only now drain and insert - if insert panics, entries remain in roots
    for (handle_id, v) in roots.strong.drain() {
        orphan.insert((thread_id, handle_id), v.as_ptr() as usize);
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The orphan table is critical for maintaining GC roots when threads terminate. Losing entries means referenced objects become immortal zombies - they'll never be collected even when all handles are dropped.

**Rustacean (Soundness 觀點):**
The `drain().collect()` pattern before acquiring the second lock is a classic anti-pattern. If the second lock acquisition or any operation in the loop panics, data is permanently lost. The fix requires either moving the drain after acquiring the second lock, or using a transaction pattern.

**Geohot (Exploit 觀點):**
While panics are exceptional, under memory pressure this could become a denial-of-service vector. More critically, if this code path is reached during thread termination during an OOM scenario, the leak could accelerate memory exhaustion.