# [Bug]: Gc::downgrade Missing Generation Check Before inc_weak (TOCTOU)

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Requires concurrent lazy sweep to reclaim and reuse slot between is_allocated check and inc_weak |
| **Severity (嚴重程度)** | Critical | Weak reference count corruption - inc_weak called on wrong object |
| **Reproducibility (復現難度)** | High | Requires precise concurrent timing |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `ptr.rs` - `Gc::downgrade()`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

### 預期行為
`Gc::downgrade()` 應在調用 `inc_weak()` 之前驗證物件槽位的 generation，確保在 `is_allocated` 檢查和 `inc_weak` 調用之間 slot 沒有被回收並重新分配。這可以防止對錯誤物件調用 `inc_weak`。

### 實際行為
`Gc::downgrade()` 存在與 bug351 相同的 TOCTOU 漏洞，但 bug351 只修復了 `GcHandle::downgrade()`，沒有修復 `Gc::downgrade()`：

1. Lines 1747-1753: 檢查 `is_allocated`
2. Line 1762: 調用 `inc_weak()` - **沒有 generation 預檢查！**
3. Lines 1764-1771: 再次檢查 `is_allocated` - **太晚了！**

在步驟 1 和步驟 2 之間，slot 可能被 sweep 回收並分配新物件（帶有不同的 generation）。然後 `inc_weak()` 會調用到新物件上，導致 weak reference count 腐壞。

---

## 🔬 根本原因分析 (Root Cause Analysis)

在 `ptr.rs:1742-1776` (`Gc::downgrade`):

```rust
// Step 1: Check is_allocated (lines 1747-1753)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
    assert!(
        (*header.as_ptr()).is_allocated(idx),
        "Gc::downgrade: slot has been swept and reused"
    );
}

// Step 2: Check dead/dropping/under_construction (lines 1756-1761)
assert!(
    !(*gc_box_ptr).has_dead_flag()
        && (*gc_box_ptr).dropping_state() == 0
        && !(*gc_box_ptr).is_under_construction(),
    ...
);

// Step 3: inc_weak - NO generation check before this!
(*gc_box_ptr).inc_weak();  // LINE 1762

// Step 4: is_allocated check AFTER inc_weak - TOO LATE! (lines 1764-1771)
if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr as *const u8) {
    ...
}
```

此問題與 bug351 相同，但 bug351 只修復了 `GcHandle::downgrade()` (cross_thread.rs:412-426)，沒有修復 `Gc::downgrade()`。

對比已修復的 `GcHandle::downgrade` (cross_thread.rs:412-426):
```rust
// Get generation BEFORE inc_weak to detect slot reuse (bug351).
let pre_generation = (*self.ptr.as_ptr()).generation();

(*self.ptr.as_ptr()).inc_weak();

// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != (*self.ptr.as_ptr()).generation() {
    (*self.ptr.as_ptr()).dec_weak();
    ...
}
```

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

```rust
// This requires concurrent testing environment
// 1. Create a Gc object
// 2. Trigger lazy sweep to reclaim the object
// 3. Allocate new object in same slot (different generation)
// 4. Concurrently call Gc::downgrade()
// 5. Observe weak_count corruption on wrong object
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `inc_weak()` 之前添加 generation 檢查，與 `GcHandle::downgrade()` 的修復一致：

```rust
// Get generation BEFORE inc_weak to detect slot reuse.
let pre_generation = (*gc_box_ptr).generation();

(*gc_box_ptr).inc_weak();

// Verify generation hasn't changed - if slot was reused, undo inc_weak.
if pre_generation != (*gc_box_ptr).generation() {
    (*gc_box_ptr).dec_weak();
    panic!("Gc::downgrade: slot was reused between pre-check and inc_weak");
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
bug351 修復了 `GcHandle::downgrade()` 的相同問題，但忘記同時修復 `Gc::downgrade()`。這是同一個 TOCTOU 模式 - 在 is_allocated 檢查和 ref 遞增之間，slot 可能被 sweep 回收並重用。

**Rustacean (Soundness 觀點):**
對錯誤物件調用 `inc_weak` 是嚴重的記憶體安全問題。可能導致：
- 新物件的 weak_count 人為提高，可能阻止回收
- 物件的 weak_count 洩漏

**Geohot (Exploit 攻擊觀點):**
如果攻擊者可以控制配置時機：
1. 釋放原始物件
2. 快速在相同 slot 配置受控物件（不同 generation）
3. 觸發 `downgrade` 來遞增錯誤物件的 weak_count
4. 利用腐壞的 weak_count 狀態

---

## 🔗 相關 Issue

- bug351: GcHandle::downgrade missing generation check before inc_weak (Fixed)
- bug289: Gc::clone missing is_allocated check BEFORE inc_ref (Fixed)
- bug257: Gc::as_weak missing is_allocated check before inc_weak (Fixed)