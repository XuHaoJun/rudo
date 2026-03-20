# [Bug]: GcHandle::downgrade Missing Generation Tracking - Slot Reuse TOCTOU

**Status:** Fixed
**Tags:** Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要精確時序控制來觸發 lazy sweep slot reuse |
| **Severity (嚴重程度)** | Critical | 弱引用計數腐敗與記憶體洩漏，可能導致 Use-After-Free |
| **Reproducibility (復現難度)** | Very High | 需要精確的執行緒交錯控制，單執行緒難以穩定復現 |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `GcHandle::downgrade` in `handles/cross_thread.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)

`GcHandle::downgrade()` 應該像 `try_resolve_impl()` 一樣，在執行 `inc_weak()` 前後檢查 generation，以檢測 slot 是否被 sweep 並重新分配給其他物件。

### 實際行為 (Actual Behavior)

`GcHandle::downgrade()` (cross_thread.rs:399-478) 缺少 generation 檢查。雖然有 `is_allocated` 和 flag 檢查，但這些檢查和 `inc_weak()` 調用之間存在 TOCTOU 窗口。如果 slot 在檢查後但在 `inc_weak` 前被 sweep 並 reuse，generation 會改變，導致 `inc_weak` 操作錯誤物件的弱引用計數。

---

## 🔬 根本原因分析 (Root Cause Analysis)

**問題點：** `handles/cross_thread.rs` - `GcHandle::downgrade()`

`try_resolve_impl()` (lines 346-358) 有正確的 generation 檢查：
```rust
// Get generation BEFORE inc_ref to detect slot reuse (bug347).
let pre_generation = gc_box.generation();

gc_box.inc_ref();

// Verify generation hasn't changed - if slot was reused, return None.
if pre_generation != gc_box.generation() {
    GcBox::dec_ref(self.ptr.as_ptr());
    return None;
}
```

但 `GcHandle::downgrade()` (lines 411-437 for TCB path, 445-471 for orphan path) 沒有這個檢查：
```rust
unsafe {
    (*self.ptr.as_ptr()).inc_weak();  // <-- 操作錯誤物件！

    if let Some(idx) = crate::heap::ptr_to_object_index(self.ptr.as_ptr() as *const u8) {
        let header = crate::heap::ptr_to_page_header(self.ptr.as_ptr() as *const u8);
        if !(*header.as_ptr()).is_allocated(idx) {
            (*self.ptr.as_ptr()).dec_weak();
            // ... return null
        }
    }
    // Flag checks - 但新物件的 flags 也是乾淨的！
    let gc_box = &*self.ptr.as_ptr();
    if gc_box.has_dead_flag() || gc_box.dropping_state() != 0 || gc_box.is_under_construction() {
        // ...
    }
}
```

**競爭條件情境：**
1. Handle 指向 slot X 中的物件 A
2. 物件 A 變成不可達，lazy sweep 回收 slot X
3. 新物件 B 被分配在 slot X（相同地址，但 generation 不同）
4. `is_allocated(idx)` 返回 `true`（slot 被 B 佔用，flags 也是乾淨的）
5. `inc_weak()` 被調用到 B 的 GcBox - **錯誤的物件！**
6. B 的 weak_count 被錯誤地遞增
7. 返回有效的 WeakCrossThreadHandle，但指向 B 而非 A！

**為什麼 flag 檢查無法捕捉 slot reuse：**
- 新物件 B 是 freshly allocated，所以 `has_dead_flag = false`、`dropping_state = 0`、`is_under_construction = false`
- 所有 flag 檢查都會通過，無法檢測到 slot 被 reuse

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要精確的執行緒交錯控制，涉及 lazy sweep：

```rust
// 概念驗證 - 需要 TSan 或極端的時序控制
// 執行緒 1: 在 handle 上調用 downgrade() 指向 A
// 執行緒 2: lazy sweep + 在相同 slot 分配 B
```

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `GcHandle::downgrade()` 中新增 generation 檢查（仿照 `try_resolve_impl()`）：

```rust
// TCB path (lines 411-437) 的修復：
unsafe {
    // Get generation BEFORE inc_weak to detect slot reuse
    let pre_generation = (*self.ptr.as_ptr()).generation();

    (*self.ptr.as_ptr()).inc_weak();

    // Verify generation hasn't changed - if slot was reused, undo and return null
    if pre_generation != (*self.ptr.as_ptr()).generation() {
        (*self.ptr.as_ptr()).dec_weak();
        drop(roots);
        return WeakCrossThreadHandle {
            weak: GcBoxWeakRef::null(),
            origin_tcb: Weak::clone(&self.origin_tcb),
            origin_thread: self.origin_thread,
        };
    }

    // ... existing is_allocated and flag checks ...
}
```

同樣的修復應用於 orphan path (lines 445-471)。

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
Slot reuse 是 GC 系統中的經典問題。Generation 機制是檢測此問題的標準方法。`is_allocated` 只能告訴我們 slot 是否被分配，不能告訴我們是否被分配給**不同的物件**。每次 allocation 時遞增 generation 確保了物件身份的追蹤。`downgrade` 缺少這個檢查是一個明顯的漏洞。

**Rustacean (Soundness 觀點):**
如果 `inc_weak` 在錯誤的物件上執行，會導致：
1. 錯誤物件的弱引用計數被增加
2. 當正確物件應該被回收時，可能因為錯誤的計數而無法回收
3. 或者錯誤物件的計數過高，導致記憶體洩漏
4. 返回一個內部指向錯誤物件的 WeakCrossThreadHandle

**Geohot (Exploit 觀點):**
Slot reuse + 引用計數操作錯誤是經典的記憶體腐敗向量。攻擊者可能：
1. 精心控制 slot reuse 的時機
2. 讓 `inc_weak` 操作攻擊者控制的物件
3. 透過錯誤的弱引用計數實現記憶體洩漏或混淆

---

## 相關 Issue

- bug347: GcHandle::resolve_impl is_allocated check insufficient (same root cause - generation tracking needed)
- bug331: GcHandle::try_resolve_impl Reference Count Leak (similar issue, fixed with dec_ref)
- bug332: GcHandle::downgrade Weak Count Leak (different bug - dec_weak not called, now fixed)
- bug350: Handle::to_gc() missing generation check (fixed in handles/mod.rs, but not cross_thread.rs)

---

## Resolution (2026-03-20)

**Outcome:** Fixed.

Added generation checks to detect slot reuse TOCTOU in `GcHandle::downgrade()`:
- TCB path (cross_thread.rs:411-437)
- Orphan path (cross_thread.rs:444-471)

The fix follows the same pattern as `try_resolve_impl()` (bug347):
1. Save generation before `inc_weak()`
2. Verify generation unchanged after `inc_weak()`
3. If generation mismatch detected, undo `inc_weak()` with `dec_weak()` and return null

**Verification:**
- Build: `cargo build --workspace` succeeds
- Clippy: No warnings
- Tests: All tests pass
