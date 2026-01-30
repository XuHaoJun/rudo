# rudo-gc + Tokio Async/Await 整合計劃 v4

**建立日期**: 2026-01-30
**作者**: opencode
**版本**: 4.0
**基於**: v3 代碼審查

## 執行摘要

修復 v3 實作中的 critical bugs 並進行 **full redesign**。v3 的核心假設（使用 `enter_scope()` sentinel address 進行自動根追蹤）被發現是 **fundamentally broken**。

### v3 → v4 重大變更

| v3 (有問題) | v4 (修復) |
|------------|-----------|
| `GcRootGuard::enter_scope()` | **移除** - sentinel address 無法保護任何 GC 對象 |
| `GcRootScope` (future wrapper) | **移除** - 沒有實際保護作用 |
| `gc::spawn()` | **移除** - 無法自動保護 Gc 指標 |
| `#[gc::root]` macro | **移除** - 需要明確傳入 Gc 指標 |

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

---

## v4 架構設計

### 保留的 API

| API | 說明 |
|-----|------|
| `GcRootSet::global()` | 程序級 singleton |
| `GcRootGuard::new(ptr)` | 保護真實 Gc 指標 |
| `GcTokioExt::root_guard()` | 用戶友好的 API |
| `GcTokioExt::yield_now()` | GC safepoint |
| `#[gc::main]` | Runtime 初始化 |

### 移除的 API

| API | 移除原因 |
|-----|---------|
| `GcRootGuard::enter_scope()` | Sentinel address 無法保護任何對象 |
| `GcRootScope<F>` | Future wrapper 沒有實際保護作用 |
| `gc::spawn()` | 無法自動保護 Gc 指標 |
| `#[gc::root]` | 依賴 broken 的 enter_scope() |

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

**2.1 修復 `unregister()` count bug** (`root.rs:75-84`):
```rust
pub fn unregister(&self, ptr: usize) {
    let mut roots = self.roots.lock().unwrap();
    let found = if let Some(pos) = roots.iter().position(|&p| p == ptr) {
        roots.swap_remove(pos);
        true
    } else {
        false
    };
    drop(roots);

    if found {
        self.count.fetch_sub(1, Ordering::AcqRel);
        self.dirty.store(true, Ordering::Release);
    }
}
```

**2.2 修復 `snapshot()` race** (`root.rs:104-108`):
```rust
pub fn snapshot(&self) -> Vec<usize> {
    let roots = self.roots.lock().unwrap().clone();
    self.dirty.store(false, Ordering::Release);  // 在 lock 內清除
    roots
}
```

### Phase 3: 更新 Proc-Macro

從 `rudo-gc-derive/src/lib.rs` 移除 `#[gc::root]` 導出。

### Phase 4: 更新測試

更新 `tokio_integration.rs`:
- 移除 `enter_scope()` 測試
- 移除 `GcRootScope` 測試
- 移除 `gc::spawn()` 測試
- 添加 `root_guard()` 明確使用模式的測試

### Phase 5: Verify

```bash
./clippy.sh  # 零 warnings
./test.sh    # 所有測試通過
```

---

## 文件變更清單

```
crates/rudo-gc/src/tokio/
├── mod.rs     [修改 - 移除 spawn(), 移除 GcRootScope export]
├── guard.rs   [修改 - 移除 enter_scope(), 移除 GcRootScope]
├── root.rs    [修改 - 修復 bug]
└── spawn.rs   [刪除或保留空]

crates/rudo-gc/tests/tokio_integration.rs  [修改 - 更新測試]

crates/rudo-gc-derive/src/lib.rs  [修改 - 移除 #[gc::root]]
```

---

## API 使用範例 (v4)

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
```

---

## 時間線

| 階段 | 內容 | 估計 |
|------|------|------|
| Phase 1 | 移除 Broken Code | 30 分鐘 |
| Phase 2 | 修復 Critical Bugs | 15 分鐘 |
| Phase 3 | 更新 Proc-Macro | 15 分鐘 |
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
- [ ] `GcRootSet::unregister()` 正確維護 count
- [ ] `GcRootSet::snapshot()` 沒有 race condition
