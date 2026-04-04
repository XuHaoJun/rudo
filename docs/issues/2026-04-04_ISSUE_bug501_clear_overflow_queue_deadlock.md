# [Bug]: clear_overflow_queue spin loop can deadlock if thread crashes

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | `Low` | Requires thread crash/panic while holding OVERFLOW_QUEUE_USERS counter |
| **Severity (嚴重程度)** | `Medium` | Process hangs indefinitely, requiring kill -9 |
| **Reproducibility (復現難度)** | `Very High` | Very difficult to reproduce - requires precise thread crash timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc/marker.rs` - `clear_overflow_queue` function
- **OS / Architecture:** `All` - any platform where Rust threads can crash
- **Rust Version:** `1.75.0+`
- **rudo-gc Version:** `0.8.0+`

---

## 📝 問題描述 (Description)

In `gc/marker.rs`, the `clear_overflow_queue` function uses a spin loop to wait for all overflow queue users to finish:

```rust
pub fn clear_overflow_queue() {
    let old_gen = OVERFLOW_QUEUE_CLEAR_GEN.fetch_add(1, Ordering::AcqRel);
    loop {
        let users = OVERFLOW_QUEUE_USERS.load(Ordering::Acquire);
        if users == 0 {
            break;
        }
        std::hint::spin_loop();
    }
    // ... drain queue ...
}
```

### 預期行為 (Expected Behavior)
The spin loop should complete in a bounded time since each worker thread decrements the counter when done.

### 實際行為 (Actual Behavior)
If a worker thread crashes or gets killed externally while `OVERFLOW_QUEUE_USERS > 0`, the spin loop will wait forever, causing the GC to hang.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The `OVERFLOW_QUEUE_USERS` counter is incremented in `push_overflow_work` and decremented in `process_overflow_work`. However:

1. A thread could crash between `fetch_add` and `fetch_sub`
2. The thread's TCB might be dropped without decrementing
3. `clear_overflow_queue` would spin forever waiting for the count to reach 0

While `Weak<ThreadControlBlock>` is used to detect thread liveness in other parts of the codebase (see `cross_thread.rs`), the overflow queue user count doesn't have such validation.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Enable parallel marking with multiple worker threads
2. Have one worker thread push overflow work
3. Kill that thread externally (e.g., SIGKILL) after `fetch_add` but before `fetch_sub`
4. Call `clear_overflow_queue` - it will spin forever

```rust
// Pseudo-PoC - actual implementation would require unsafe and process signals
fn poc_crash_thread() {
    let handle = std::thread::spawn(|| {
        // This increments OVERFLOW_QUEUE_USERS
        push_overflow_work(node);
        // If process is killed here, counter is not decremented
    });
    // External SIGKILL to handle...
    clear_overflow_queue(); // Hangs!
}
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Option 1: Add a timeout to the spin loop:
```rust
use std::time::{Duration, Instant};
let timeout = Duration::from_secs(5);
let start = Instant::now();
loop {
    let users = OVERFLOW_QUEUE_USERS.load(Ordering::Acquire);
    if users == 0 {
        break;
    }
    if start.elapsed() > timeout {
        // Log warning and proceed with drain anyway, or panic
        break;
    }
    std::hint::spin_loop();
}
```

Option 2: Use per-thread tracking with TCB weak reference validation:
```rust
// Instead of simple counter, track per-thread state
static OVERFLOW_QUEUE_USERS: Mutex<HashMap<ThreadId, bool>> = Mutex::new(HashMap::new());
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The spin loop without timeout is a classic GC implementation anti-pattern. In Chez Scheme, we use explicit handshakes with worker threads or bounded waits. The unbounded spin can cause GC to hang when the system is under memory pressure and threads are being killed by OOM killer.

**Rustacean (Soundness 觀點):**
No soundness issue - this is liveness degradation (deadlock) not memory safety violation. However, the process becoming unresponsive could prevent cleanup of other resources.

**Geohot (Exploit 觀點):**
An attacker could intentionally trigger OOM conditions causing thread kills, leading to GC deadlock and denial of service. This could be used to freeze the process in a crashed-like state.
