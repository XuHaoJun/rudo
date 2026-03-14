# [Bug]: HashMap Trace implementation has misleading comment

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | N/A | Documentation bug only |
| **Severity (嚴重程度)** | Low | Misleading documentation, no functional impact |
| **Reproducibility (復現難度)** | N/A | Documentation issue |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Trace` implementation for `HashMap` in `trace.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
Documentation should accurately describe what the implementation does.

### 實際行為
The `Trace` implementation for `HashMap` has a misleading comment:

```rust
// SAFETY: HashMap traces all values (keys assumed to not contain Gc)
unsafe impl<K: Trace, V: Trace, S: BuildHasher> Trace for HashMap<K, V, S> {
    fn trace(&self, visitor: &mut impl Visitor) {
        for (k, v) in self {
            k.trace(visitor);  // <-- This traces keys!
            v.trace(visitor);
        }
    }
}
```

The comment says "keys assumed to not contain Gc" but the implementation actually traces both keys AND values (`k.trace(visitor)` at line 463).

This is inconsistent and could confuse developers:
1. The comment implies keys don't contain Gc (are not traced)
2. But the implementation explicitly traces keys with `k.trace(visitor)`

---

## 🔬 根本原因分析 (Root Cause Analysis)

The comment was likely written when the implementation only traced values, but then someone correctly updated the implementation to also trace keys (for cases where HashMap keys do contain Gc pointers). However, they forgot to update the comment.

Compare with `GcCapture` implementation in `cell.rs` which correctly captures both keys and values without any misleading comments:

```rust
impl<K: GcCapture + 'static, V: GcCapture + 'static, S: std::hash::BuildHasher + Default> GcCapture
    for HashMap<K, V, S>
{
    fn capture_gc_ptrs_into(&self, ptrs: &mut Vec<NonNull<GcBox<()>>>) {
        for key in self.keys() {
            key.capture_gc_ptrs_into(ptrs);
        }
        for value in self.values() {
            value.capture_gc_ptrs_into(ptrs);
        }
    }
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

Read the comment in `trace.rs:458` and compare with the implementation at `trace.rs:462-464`.

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

Update the comment to accurately reflect the implementation:

```rust
// SAFETY: HashMap traces both keys and values
unsafe impl<K: Trace, V: Trace, S: BuildHasher> Trace for HashMap<K, V, S> {
```

Or simply remove the comment about keys.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
From a GC perspective, tracing both keys and values is correct behavior - HashMap keys can legitimately contain Gc pointers. The comment should reflect this.

**Rustacean (Soundness 觀點):**
This is a documentation bug only. The implementation is correct - it properly traces both keys and values. The comment is just misleading.

**Geohot (Exploit 觀點):**
No security impact - this is purely a documentation issue.
