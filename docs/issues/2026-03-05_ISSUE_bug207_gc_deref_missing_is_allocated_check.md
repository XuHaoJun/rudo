# [Bug]: Gc::deref 缺少 is_allocated 檢查導致 Slot Reuse 後存取錯誤物件

**Status:** Open
**Tags:** Unverified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | 需要 lazy sweep 與 Gc deref 並發執行，slot 被回收並重新分配 |
| **Severity (嚴重程度)** | Critical | 會存取到錯誤物件的資料，導致記憶體錯誤 |
| **Reproducibility (復現難度)** | High | 需要精確的時序控制來觸發 slot reuse |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Gc::deref()` (ptr.rs:1572-1585)
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Latest

---

## 📝 問題描述 (Description)

`Gc::deref()` 在解引用指標之前沒有檢查 `is_allocated`。當 slot 被 lazy sweep 回收並重新分配給新物件時，舊的 Gc指標會存取到新物件的資料，導致資料混淆。

此問題與 bug197 (Gc 核心方法缺少 is_allocated 檢查) 為同一模式，但 bug197 的列表中未包含 `deref`。

### 預期行為 (Expected Behavior)

在解引用前應檢查 `is_allocated`，若 slot 已被回收並重新分配則應 panic。

### 實際行為 (Actual Behavior)

`Gc::deref()` 實現 (ptr.rs:1572-1585):
```rust
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::deref: cannot dereference a null Gc");
    let gc_box_ptr = ptr.as_ptr();
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && (*gc_box_ptr).is_under_construction(),
            "Gc::deref: cannot dereference a dead, dropping, or under construction Gc"
        );
        &(*gc_box_ptr).value  // <-- 沒有 is_allocated 檢查!
    }
}
```

對比 bug197 中的 `Gc::as_ptr()` 修復建議:
```rust
// 添加 is_allocated 檢查
let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
if let Some(header) = header {
    let index = /* 計算物件索引 */;
    assert!(
        (*header.as_ptr()).is_allocated(index),
        "Gc::deref: slot has been swept and reused"
    );
}
```

---

## 🔬 根本原因分析 (Root Cause Analysis)

當 slot reuse 與 deref 並發執行時：
1. 物件 A 在 slot X 被分配，使用者持有 `Gc<A>` 指針 P1
2. 物件 A 被 drop，ref_count 歸零
3. Lazy sweep 回收 slot X，物件 A 的記憶體被釋放
4. 新物件 B 在同一 slot X 被分配（記憶體位址相同）
5. 使用者 dereference 舊的 Gc 指針 P1
6. **BUG:** P1 現在指向物件 B 的資料，而非物件 A！

由於指標只檢查了 `has_dead_flag()`, `dropping_state()`, `is_under_construction()`，這些 flag 在新物件 B 都是正常值，所以檢查會通過，導致存取到錯誤的物件。

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

需要並發測試：
1. 建立 Gc 物件 A
2. 保存 Gc 指針 P1
3. 觸發 lazy sweep 回收物件 A
4. 在同一 slot 分配新物件 B
5. dereference P1 - 觀察是否取得 B 的資料而非 A

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

在 `Gc::deref()` 中添加 `is_allocated` 檢查，參考 bug197 的修復模式:

```rust
fn deref(&self) -> &Self::Target {
    let ptr = self.ptr.load(Ordering::Acquire);
    assert!(!ptr.is_null(), "Gc::deref: cannot dereference a null Gc");
    let gc_box_ptr = ptr.as_ptr();
    
    // 添加 is_allocated 檢查以防止 slot reuse 導致的錯誤物件存取
    unsafe {
        let header = crate::heap::ptr_to_page_header(gc_box_ptr as *const u8);
        if let Some(h) = header {
            if let Some(idx) = crate::heap::ptr_to_object_index(gc_box_ptr.cast()) {
                assert!(
                    (*h.as_ptr()).is_allocated(idx),
                    "Gc::deref: slot has been swept and reused"
                );
            }
        }
    }
    
    unsafe {
        assert!(
            !(*gc_box_ptr).has_dead_flag()
                && (*gc_box_ptr).dropping_state() == 0
                && !(*gc_box_ptr).is_under_construction(),
            "Gc::deref: cannot dereference a dead, dropping, or under construction Gc"
        );
        &(*gc_box_ptr).value
    }
}
```

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
這與 bug197 描述的問題相同，只是影響範圍擴展到 `deref`。Slot reuse 會導致舊指標指向新物件，這在 GC 系統中是經典的記憶體安全問題。

**Rustacean (Soundness 觀點):**
這是嚴重的記憶體安全問題。使用者期望存取物件 A，實際卻存取到物件 B，導致資料混淆。

**Geohot (Exploit 攻擊觀點):**
攻擊者可以嘗試控制 slot reuse 的時序，透過精確的執行緒調度讓舊指標指向攻擊者控制的物件，實現任意記憶體讀寫。

---

## 🔗 相關 Issue

- bug197: Gc 核心方法 (as_ptr, internal_ptr, etc.) 缺少 is_allocated 檢查
- bug206: GcHandle::resolve/try_resolve/clone 缺少 inc_ref 後的 is_allocated 檢查

---

## 修復狀態

- [ ] 已修復
- [x] 未修復
