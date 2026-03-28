# [Bug]: Gc::weak_cross_thread_handle Missing Generation Check Before inc_weak (TOCTOU)

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
- **Component:** `ptr.rs` - `Gc::weak_cross_thread_handle()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Gc::weak_cross_thread_handle()` 應在調用 `inc_weak()` 之前驗證物件槽位的 generation，確保在 `is_allocated` 檢查和 `inc_weak` 調用之間 slot 沒有被回收並重新分配。這可以防止對錯誤物件調用 `inc_weak`。

### 實際行為 (Actual Behavior)
`Gc::weak_cross_thread_handle()` 存在與 bug356 相同的 TOCTOU 漏洞，但 bug356 只修復了 `Gc::downgrade()`，沒有修復 `Gc::weak_cross_thread_handle()`：

1. Lines 1962-1969: 檢查 `is_allocated`
2. Line 1977: 調用 `inc_weak()` - **沒有 generation 預檢查！**
3. Lines 1979-1986: 再次檢查 `is_allocated` - **太晚了！**

在步驟 1 和步驟 2 之間，slot 可能被 sweep 回收並分配新物件（帶有不同的 generation）。然後 `inc_weak()` 會調用到新物件上，導致 weak reference count 腐壞。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1958-1996` (`Gc::weak_cross_thread_handle`):

```rust
// Step 1: Check is_allocated (lines 1962-1969)
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    let header = crate::heap::ptr_to_page_header(ptr.as_ptr() as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::weak_cross_thread_handle: slot has been swept and reused"
    );
}

// Step 2: Check dead/dropping/under_construction (lines 1970-1976)
let gc_box = &*ptr.as_ptr();
assert!(
    !gc_box.has_dead_flag()
        && gc_box.dropping_state() == 0
        && !gc_box.is_under_construction(),
    ...
);

// Step 3: inc_weak - NO generation check before this!
gc_box.inc_weak();  // LINE 1977

// Step 4: is_allocated check AFTER inc_weak - TOO LATE! (lines 1979-1986)
if let Some(idx) = crate::heap::ptr_to_object_index(ptr.as_ptr() as *const u8) {
    ...
}
```

此問題與 bug356 相同，但 bug356 只修復了 `Gc::downgrade()` (ptr.rs:1745-1788)，沒有修復 `Gc::weak_cross_thread_handle()`。

對比已修復的 `Gc::downgrade` (ptr.rs:1763-1773):
```rust
// Get generation BEFORE inc_weak to detect slot reuse (bug356).
let pre_generation = (*gc_box_ptr).generation();

(*gc_box_ptr).inc_weak();

// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != (*gc_box_ptr).generation() {
    (*gc_box_ptr).dec_weak();
    panic!("Gc::downgrade: slot was reused between pre-check and inc_weak");
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This requires concurrent testing environment
// 1. Create a Gc object
// 2. Trigger lazy sweep to reclaim the object
// 3. Allocate new object in same slot (different generation)
// 4. Concurrently call Gc::weak_cross_thread_handle()
// 5. Observe weak_count corruption on wrong object
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_weak()` 之前添加 generation 檢查，與 `Gc::downgrade()` 的修復一致：

```rust
// Get generation BEFORE inc_weak to detect slot reuse.
let pre_generation = gc_box.generation();

gc_box.inc_weak();

// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != gc_box.generation() {
    gc_box.dec_weak();
    panic!("Gc::weak_cross_thread_handle: slot was reused between pre-check and inc_weak");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
bug356 修復了 `Gc::downgrade()` 的相同問題，但忘記同時修復 `Gc::weak_cross_thread_handle()`。這是同一個 TOCTOU 模式 - 在 is_allocated 檢查和 ref 遞增之間，slot 可能被 sweep 回收並重用。

**Rustacean (Soundness 觀點):**
對錯誤物件調用 `inc_weak` 是嚴重的記憶體安全問題。可能導致：
- 新物件的 weak_count 人為提高，可能阻止回收
- 物件的 weak_count 洩漏

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制配置時機：
1. 釋放原始物件
2. 快速在相同 slot 配置受控物件（不同 generation）
3. 觸發 `weak_cross_thread_handle` 來遞增錯誤物件的 weak_count
4. 利用腐壞的 weak_count 狀態

---

## 🔗 相關 Issue

- bug356: Gc::downgrade missing generation check before inc_weak (Fixed)
- bug351: GcHandle::downgrade missing generation check before inc_weak (Fixed)
- bug257: Gc::as_weak/weak_cross_thread_handle missing is_allocated check (Fixed - partial)
