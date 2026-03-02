# [Bug]: gc_cell_validate_and_barrier large object boundary check off-by-h_size

**Status:** Open
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Only affects large objects (those in `large_object_map`), which are less common than regular allocations |
| **Severity (嚴重程度)** | Low | Incorrect barrier range could cause unnecessary barrier firings but not memory corruption |
| **Reproducibility (復現難度)** | High | Requires specific large object allocation pattern and pointer in the incorrect range |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `gc_cell_validate_and_barrier` in `heap.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** 0.8+

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
The boundary check should validate that `ptr_addr` is within the actual object range:
- Object starts at: `head_addr + h_size` (after header)
- Object ends at: `head_addr + h_size + size`

### 實際行為 (Actual Behavior)
The boundary check incorrectly uses `head_addr + size` instead of `head_addr + h_size + size`:
```rust
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + size {  // BUG
```

This means pointers in the range `[head_addr + size, head_addr + h_size + size)` will incorrectly pass the check.

---

## 🔬 根本原因分析 (Root Cause Analysis)

The bug is in `crates/rudo-gc/src/heap.rs` line 2780. The function `gc_cell_validate_and_barrier` has an off-by-`h_size` error in the boundary check:

**Buggy code (line 2780):**
```rust
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + size {
```

**Correct code (as seen in `unified_write_barrier` at line 2897):**
```rust
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + h_size + size {
```

The `large_object_map` stores `(header_addr, object_size, header_size)`:
- `head_addr`: start of page where PageHeader is located
- `h_size`: size of the header
- `size`: size of the object data

The object data spans from `head_addr + h_size` to `head_addr + h_size + size`, but the buggy code checks against `head_addr + size` instead.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

1. Allocate a large object (larger than page size, stored in `large_object_map`)
2. Get a pointer in the range `[head_addr + size, head_addr + h_size + size)`
3. Call `gc_cell_validate_and_barrier` with that pointer
4. Observe that the check incorrectly passes when it should fail

Note: This is difficult to reproduce directly as it requires specific memory layout knowledge.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Change line 2780 in `crates/rudo-gc/src/heap.rs`:

From:
```rust
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + size {
```

To:
```rust
if ptr_addr < head_addr + h_size || ptr_addr >= head_addr + h_size + size {
```

This matches the correct check in `unified_write_barrier` at line 2897.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
This is a classic off-by-N boundary error in the write barrier. The barrier is critical for generational GC correctness - it ensures old-to-young references are tracked. While the impact may be limited (only affects large objects), this kind of subtle boundary bug can cause intermittent and hard-to-reproduce issues. The fact that the identical check in `unified_write_barrier` is correct suggests this was likely a copy-paste error during development.

**Rustacean (Soundness 觀點):**
This is not technically undefined behavior since the check is just for early-return optimization. However, it's a logic error that could lead to incorrect program behavior. The function `gc_cell_validate_and_barrier` is called from `GcCell::borrow_mut()` which is a hot path. While the bug won't cause memory corruption directly, it could cause the GC to track incorrect dirty pages.

**Geohot (Exploit 觀點):**
This bug alone isn't directly exploitable as a security vulnerability - it won't cause buffer overflows or use-after-free. However, in combination with other GC bugs or in adversarial conditions (e.g., specific memory layout), this could potentially be part of a chain that leads to incorrect memory management. The fact that the barrier might fire for invalid memory regions is concerning from a defensive coding perspective.
