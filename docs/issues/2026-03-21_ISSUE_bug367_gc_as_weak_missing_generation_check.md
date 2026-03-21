# [Bug]: Gc::as_weak Missing Generation Check Before inc_weak (TOCTOU)

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep to reclaim and reuse slot between is_allocated check and inc_weak |
| **Severity (嚴重程度)** | Critical | Weak reference count corruption - inc_weak called on wrong object |
| **Reproducibility (復現難度)** | High | Requires precise concurrent timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ptr.rs` - `Gc::as_weak()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Gc::as_weak()` 應在調用 `inc_weak()` 之前驗證物件槽位的 generation，確保在 `is_allocated` 檢查和 `inc_weak` 調用之間 slot 沒有被回收並重新分配。

### 實際行為 (Actual Behavior)
`Gc::as_weak()` 存在與 bug366 相同的 TOCTOU 漏洞：

1. Lines 1821-1828: 檢查 `is_allocated`
2. Lines 1830-1838: 檢查 state flags
3. Line 1839: 調用 `inc_weak()` - **沒有 generation 預檢查！**
4. Lines 1841-1848: 再次檢查 `is_allocated`

在步驟 1 和步驟 3 之間，slot 可能被 sweep 回收並分配新物件（帶有不同的 generation）。然後 `inc_weak()` 會調用到新物件上，導致 weak reference count 腐壞。

bug366 修復了 `Gc::weak_cross_thread_handle()` 的相同問題，但遺漏了 `Gc::as_weak()`。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1813-1854` (`Gc::as_weak`):

```rust
// Step 1: Check is_allocated (lines 1821-1828)
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    if !(*header.as_ptr()).is_allocated(idx) {
        return GcBoxWeakRef { ptr: AtomicNullable::null() };
    }
}

// Step 2: Check flags (lines 1830-1838)
let gc_box = &*ptr.as_ptr();
if gc_box.is_under_construction()
    || gc_box.has_dead_flag()
    || gc_box.dropping_state() != 0
{
    return GcBoxWeakRef { ptr: AtomicNullable::null() };
}

// Step 3: inc_weak - NO generation check before this!
(*ptr.as_ptr()).inc_weak();  // LINE 1839 - BUG!

// Step 4: is_allocated check AFTER inc_weak (lines 1841-1848)
```

對比已修復的 `Gc::weak_cross_thread_handle` (lines 1977-1984):
```rust
let pre_generation = gc_box.generation();
gc_box.inc_weak();
if pre_generation != gc_box.generation() {
    gc_box.dec_weak();
    panic!("Gc::weak_cross_thread_handle: slot was reused...");
}
```

`Gc::as_weak()` 缺少這個 generation 檢查。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This requires concurrent testing environment
// 1. Create a Gc object
// 2. Trigger lazy sweep to reclaim the object
// 3. Allocate new object in same slot (different generation)
// 4. Concurrently call Gc::as_weak()
// 5. Observe weak_count corruption on wrong object
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_weak()` 之前添加 generation 檢查：

```rust
// Get generation BEFORE inc_weak to detect slot reuse.
let gc_box = &*ptr.as_ptr();
let pre_generation = gc_box.generation();

(*ptr.as_ptr()).inc_weak();

// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != (*ptr.as_ptr()).generation() {
    (*ptr.as_ptr()).dec_weak();
    return GcBoxWeakRef { ptr: AtomicNullable::null() };
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
bug366 修復了 `Gc::weak_cross_thread_handle()` 的相同問題，但 `Gc::as_weak()` 被遺漏。這是同一個 TOCTOU 模式 - 在 is_allocated 檢查和 ref 遞增之間，slot 可能被 sweep 回收並重用。

**Rustacean (Soundness 觀點):**
對錯誤物件調用 `inc_weak` 是嚴重的記憶體安全問題。可能導致新物件的 weak_count 人為提高，阻止正確回收。

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制配置時機，可以操縱 weak_count 腐壞狀態。

---

## 🔗 相關 Issue

- bug366: Gc::weak_cross_thread_handle missing generation check before inc_weak (Fixed)
- bug356: Gc::downgrade missing generation check before inc_weak (Fixed)
- bug257: Gc::as_weak missing is_allocated check before inc_weak (Fixed - partial, missing generation check)
