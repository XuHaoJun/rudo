# [Bug]: GcTokioExt::yield_now() doesn't integrate with incremental marking

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Users following documentation to call yield_now in async loops |
| **Severity (嚴重程度)** | Medium | Memory buildup in long-running async tasks; GC not progressing |
| **Reproducibility (復現難度)** | Medium | Single-threaded tokio runtime + incremental marking active |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcTokioExt::yield_now()` (tokio/mod.rs)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`GcTokioExt::yield_now()` should integrate with incremental marking, similar to `Gc::yield_now()`, to allow GC to make progress during long-running async computations.

### 實際行為 (Actual Behavior)
`GcTokioExt::yield_now()` only calls `tokio::task::yield_now().await` without performing any incremental marking work. In single-threaded tokio runtime or when GC worker threads are busy, this means no GC progress is made despite the documentation promising "allows the GC to run during long-running computations."

---

## 🔬 根本原因分析 (Root Cause Analysis)

**Sync version** (`lib.rs:159-167`):
```rust
pub fn yield_now() {
    if crate::gc::incremental::is_incremental_marking_active() {
        let config = get_incremental_config();
        let budget = config.increment_size;
        crate::heap::with_heap(|heap| {
            let _ = crate::gc::incremental::incremental_mark_slice(heap, budget);
        });
    }
}
```

**Async version** (`tokio/mod.rs:129-131`):
```rust
async fn yield_now(&self) {
    task::yield_now().await;
}
```

The async version was designed to only yield to the tokio scheduler, relying on GC running in other threads. However, this doesn't advance incremental marking when:
1. Using single-threaded tokio runtime (common in tests)
2. GC worker threads are blocked/busy
3. User expects cooperative GC behavior like sync version

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
#[tokio::test(flavor = "current_thread")]
fn test_yield_now_incremental_marking() {
    // Enable incremental marking
    rudo_gc::gc::incremental::set_incremental_config(
        rudo_gc::gc::incremental::IncrementalConfig {
            enabled: true,
            ..Default::default()
        }
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    rt.block_on(async {
        let gc = Gc::new(TestData { value: 42 });

        // Call yield_now repeatedly
        for _ in 0..100 {
            gc.yield_now().await;
        }

        // With sync version, incremental marking would progress
        // With async version, no marking work is done
    });
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Modify `GcTokioExt::yield_now()` to integrate with incremental marking before yielding:

```rust
async fn yield_now(&self) {
    // Perform incremental marking if active (same as sync version)
    if crate::gc::incremental::is_incremental_marking_active() {
        let config = crate::gc::get_incremental_config();
        let budget = config.increment_size;
        crate::heap::with_heap(|heap| {
            let _ = crate::gc::incremental::incremental_mark_slice(heap, budget);
        });
    }
    // Then yield to tokio scheduler
    task::yield_now().await;
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The sync `yield_now()` was designed as a safepoint that performs incremental marking work. The async version should mirror this behavior for consistency. In multi-threaded runtimes, performing marking before yield ensures work is done before other threads get CPU time.

**Rustacean (Soundness 觀點):**
No soundness issues - the fix simply adds a check before yielding. The `is_incremental_marking_active()` is already used safely in the sync version.

**Geohot (Exploit 觀點):**
No exploit potential. This is a correctness bug where GC doesn't progress as expected, potentially leading to memory buildup, not a security issue.
