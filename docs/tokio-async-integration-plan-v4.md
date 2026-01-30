# rudo-gc + Tokio Async/Await 整合計劃 v4

**建立日期**: 2026-01-30
**作者**: opencode
**版本**: 4.1
**基於**: v3 代碼審查 + v4 實作修正

## 執行摘要

修復 v3 實作中的 critical bugs 並進行 **full redesign**。v3 的核心假設（使用 `enter_scope()` sentinel address 進行自動根追蹤）被發現是 **fundamentally broken**。

v4.1 更新：基於 Code Review 發現了 **最嚴重的 bug** - `GcRootSet` 與 GC marking 完全脫節。新增 GC 整合、移除不可靠的 count 欄位、`snapshot()` 增加指標驗證。

### v3 → v4.1 重大變更

| v3 (有問題) | v4.1 (修復) |
|------------|-----------|
| `GcRootGuard::enter_scope()` | **移除** - sentinel address 無法保護任何 GC 對象 |
| `GcRootScope` (future wrapper) | **移除** - 沒有實際保護作用 |
| `gc::spawn()` | **移除** - 無法自動保護 Gc 指標 |
| `#[gc::root]` macro | **移除** - 需要明確傳入 Gc 指標 |
| `GcRootSet::count()` | **移除** - race-prone，改用 `len()` |
| `GcRootSet::snapshot()` | **修改** - 現在需要 `&LocalHeap` 參數 |
| `GcRootSet::unregister()` | **修改** - 使用 `retain()` 而非 `swap_remove()` |
| **無 GC 整合** | **新增** - tokio roots 現在會被 GC tracing |

---

## 代碼審查發現的 Critical Bugs

### Bug 1: Duplicate Type Definition
`GcRootScope` 在兩處定義：`guard.rs:167` 和 `spawn.rs:17` → 編譯錯誤

### Bug 2: Count Decrement Always Runs
`root.rs:82` 總是遞減 count，即使 ptr 不存在

### Bug 3: Scope Guard Address Collision
`enter_scope()` 總是使用 `NonNull::dangling()` → 所有 scopes 碰撞

### Bug 4: spawn() Uses Dangling Pointer
`mod.rs:177` 創建的 guard 不保護任何實際 Gc 指標

### Bug 5: GcRootSet 與 GC 完全脫節 (v4.1 發現)
`GcRootSet::snapshot()` **從未被調用**，tokio roots 完全不會被 GC tracing。

---

## v4.1 架構設計

### 保留的 API

| API | 說明 |
|-----|------|
| `GcRootSet::global()` | 程序級 singleton |
| `GcRootGuard::new(ptr)` | 保護真實 Gc 指標 |
| `GcTokioExt::root_guard()` | 用戶友好的 API |
| `GcTokioExt::yield_now()` | GC safepoint |
| `#[gc::main]` | Runtime 初始化 |
| `GcRootSet::len()` | 新增 - 取得 root 數量 |
| `GcRootSet::is_empty()` | 新增 - 檢查是否為空 |
| `GcRootSet::clear_dirty()` | 新增 - 測試用，清除 dirty flag |
| `GcRootSet::snapshot(heap)` | 修改 - 需要 `&LocalHeap` 參數 |

### 移除的 API

| API | 移除原因 |
|-----|---------|
| `GcRootGuard::enter_scope()` | Sentinel address 無法保護任何對象 |
| `GcRootScope<F>` | Future wrapper 沒有實際保護作用 |
| `gc::spawn()` | 無法自動保護 Gc 指標 |
| `#[gc::root]` | 依賴 broken 的 enter_scope() |
| `GcRootSet::count()` | Race-prone，改用 `len()` |

---

## 實作階段

### Phase 1: 移除 Broken Code

| Task | File | Change |
|------|------|--------|
| 移除 `GcRootScope` | `guard.rs:167-199` | 刪除 struct + impls |
| 移除 `GcRootScope` | `spawn.rs:17-46` | 刪除 struct + impls |
| 移除 `enter_scope()` | `guard.rs:59-81` | 刪除方法 |
| 移除 `spawn()` | `mod.rs:172-183` | 刪除函數 |
| 更新 export | `mod.rs:43` | `pub use guard::{GcRootGuard}` |

### Phase 2: 修復 Critical Bugs

**2.1 移除 `count` 欄位，改用 `len()` 和 `is_empty()`**:
```rust
// 舊 (有 race):
pub struct GcRootSet {
    roots: Mutex<Vec<usize>>,
    count: AtomicUsize,  // 移除
    dirty: AtomicBool,
}

pub fn count(&self) -> usize {
    self.count.load(Ordering::Acquire)  // 移除
}

pub fn len(&self) -> usize {
    self.roots.lock().unwrap().len()  // 新增
}
```

**2.2 `unregister()` 使用 `retain()` 而非 `swap_remove()`**:
```rust
pub fn unregister(&self, ptr: usize) {
    let mut roots = self.roots.lock().unwrap();
    let was_present = roots.contains(&ptr);
    if was_present {
        roots.retain(|&p| p != ptr);  // 避免 swap_remove 造成的順序問題
    }
    drop(roots);

    if was_present {
        self.dirty.store(true, Ordering::Release);
    }
}
```

**2.3 `snapshot()` 增加 heap 參數和指標驗證** (Critical Fix):
```rust
pub fn snapshot(&self, heap: &crate::heap::LocalHeap) -> Vec<usize> {
    let roots = self.roots.lock().unwrap();
    let valid_roots: Vec<usize> = roots
        .iter()
        .filter(|&&ptr| {
            // 驗證指標是否為有效的 GcBox
            unsafe { crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8).is_some() }
        })
        .copied()
        .collect();
    drop(roots);
    self.dirty.store(false, Ordering::Release);
    valid_roots
}
```

**2.4 新增 `clear_dirty()` 方法** (測試用):
```rust
pub fn clear_dirty(&self) {
    self.dirty.store(false, Ordering::Release);
}
```

### Phase 3: GC 整合 (v4.1 新增 - Critical)

在 `gc/gc.rs` 的三個 GC 路徑中加入 tokio root tracing：

**Minor GC** (line ~670):
```rust
#[cfg(feature = "tokio")]
#[allow(clippy::explicit_iter_loop)]
{
    use crate::tokio::GcRootSet;
    for &ptr in GcRootSet::global().snapshot(heap).iter() {
        unsafe {
            if let Some(gc_box) = crate::heap::find_gc_box_from_ptr(heap, ptr as *const u8) {
                mark_object_minor(gc_box, &mut visitor);
            }
        }
    }
}
```

**Major GC (Parallel)** (line ~804) 和 **Generational GC** (line ~918) 同樣的模式。

### Phase 4: 更新 Proc-Macro

從 `rudo-gc-derive/src/lib.rs` 移除 `#[gc::root]` 導出。

### Phase 5: 更新測試

更新 `tokio_integration.rs`:
- 移除 `enter_scope()` 測試
- 移除 `GcRootScope` 測試
- 移除 `gc::spawn()` 測試
- 添加 `root_guard()` 明確使用模式的測試
- `count()` → `len()`，`snapshot()` → `snapshot(heap)`

更新 `tokio_multi_runtime.rs`:
- `snapshot()` → `clear_dirty()`

### Phase 6: Lint & Verify

```bash
cargo fmt --all
./clippy.sh  # 零 warnings
./test.sh    # 所有測試通過
```

---

## 文件變更清單

```
crates/rudo-gc/src/gc/
└── gc.rs    [修改 - 新增 tokio root tracing]

crates/rudo-gc/src/tokio/
├── mod.rs     [修改 - 移除 spawn(), 移除 GcRootScope export]
├── guard.rs   [修改 - 移除 enter_scope(), 移除 GcRootScope]
├── root.rs    [修改 - 重構 API，新增 snapshot(heap)]
└── spawn.rs   [保留空]

crates/rudo-gc/tests/
├── tokio_integration.rs   [修改 - 更新測試]
└── tokio_multi_runtime.rs [修改 - 更新測試]

crates/rudo-gc-derive/src/lib.rs  [修改 - 移除 #[gc::root]]
```

---

## API 使用範例 (v4.1)

```rust
use rudo_gc::{Gc, Trace, GcTokioExt};

#[derive(Trace)]
struct Data { value: i32 }

async fn example() {
    let gc = Gc::new(Data { value: 42 });

    // 明確創建 root guard (這是正確的模式)
    let _guard = gc.root_guard();

    tokio::spawn(async move {
        println!("{}", gc.value);  // gc 受保護
    }).await.unwrap();
}

// 手動創建 guard (較低層級)
use rudo_gc::tokio::GcRootGuard;
let guard = unsafe { GcRootGuard::new(gc_internal_ptr) };
```

---

## 時間線

| 階段 | 內容 | 估計 |
|------|------|------|
| Phase 1 | 移除 Broken Code | 30 分鐘 |
| Phase 2 | 修復 Critical Bugs | 30 分鐘 |
| Phase 3 | GC 整合 (新) | 15 分鐘 |
| Phase 4 | 更新測試 | 30 分鐘 |
| Phase 5 | Lint & Verify | 15 分鐘 |
| **總計** | | **~2 小時** |

---

## 驗收標準

- [ ] `./clippy.sh` 通過，零 warnings
- [ ] `./test.sh` 所有測試通過
- [ ] 移除所有 `enter_scope()` 調用
- [ ] 移除所有 `GcRootScope` 定義
- [ ] 移除 `gc::spawn()` 函數
- [ ] `GcRootSet::len()` 正確運作
- [ ] `GcRootSet::snapshot(heap)` 驗證指標並返回有效的 GcBox
- [ ] **GC 會 tracing tokio roots** - 最關鍵的驗收標準
- [ ] `GcRootSet::unregister()` 使用 `retain()` 避免順序問題
