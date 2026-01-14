# 技術規格書：`Gc::new_cyclic_weak` 實作計劃

**文件版本：** 1.0  
**日期：** 2026-01-14  
**狀態：** 草案  
**相關文件：** [cyclic-improve-1.md](./cyclic-improve-1.md)

---

## 1. 概述

### 1.1 目標

實作 `Gc::new_cyclic_weak` 方法，允許用戶建立具有自引用的垃圾回收物件，使用 `Weak<T>` 作為自引用的載體。

### 1.2 動機

現有 `Gc::new_cyclic` 因 Rust 型別系統限制無法正确工作。`Trace` trait 的唯讀語義使得 "dead" `Gc<T>` 無法被 rehydrate。本方案利用現有的 `Weak<T>` 機制提供可行的自引用支援。

### 1.3 設計原則

1. **最小變更**：僅新增一個公開方法，不修改現有 API
2. **利用現有機制**：複用已測試的 `Weak<T>` 實作
3. **型別安全**：編譯期保證型別正確性
4. **零額外開銷**：不增加普通 `Gc<T>` 的記憶體或效能成本

---

## 2. API 設計

### 2.1 新增方法簽名

```rust
impl<T: Trace> Gc<T> {
    /// Create a self-referential garbage-collected value using a Weak reference.
    ///
    /// The closure receives a `Weak<T>` that will be upgradeable after
    /// construction completes. Store this `Weak` in the constructed value
    /// and call `upgrade()` when access to the self-reference is needed.
    ///
    /// # Panics
    ///
    /// Panics if `T` is a zero-sized type (ZST).
    ///
    /// # Examples
    ///
    /// ```
    /// use rudo_gc::{Gc, Weak, Trace, GcCell};
    ///
    /// #[derive(Trace)]
    /// struct Node {
    ///     self_ref: GcCell<Option<Weak<Node>>>,
    ///     data: i32,
    /// }
    ///
    /// let node = Gc::new_cyclic_weak(|weak_self| {
    ///     Node {
    ///         self_ref: GcCell::new(Some(weak_self)),
    ///         data: 42,
    ///     }
    /// });
    ///
    /// // Access self through upgrade()
    /// let weak = node.self_ref.borrow();
    /// let self_ref = weak.as_ref().unwrap().upgrade().unwrap();
    /// assert_eq!(self_ref.data, 42);
    /// ```
    pub fn new_cyclic_weak<F>(data_fn: F) -> Self
    where
        F: FnOnce(Weak<T>) -> T;
}
```

### 2.2 型別約束

| 約束 | 說明 |
|------|------|
| `T: Trace` | 必須實作 `Trace` trait 以支援 GC 遍歷 |
| `F: FnOnce(Weak<T>) -> T` | 閉包接收 `Weak<T>` 並返回 `T` |
| `T: 'static` | 隱含必須（`Gc<T>` 要求 `T: 'static`） |

### 2.3 與現有 API 的關係

```
Gc::new(value)              // 普通建構
Gc::new_cyclic(|gc| ...)    // 已存在但不工作，標記 deprecated
Gc::new_cyclic_weak(|w| ...)// 新增，推薦使用
```

---

## 3. 實作細節

### 3.1 記憶體佈局

```
┌─────────────────────────────────────────────────────────────┐
│                         PageHeader                          │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────────────────────────────────────────────┐   │
│  │                      GcBox<T>                        │   │
│  ├─────────────────────────────────────────────────────┤   │
│  │  ref_count: Cell<NonZeroUsize>  [初始值: 1]         │   │
│  │  weak_count: Cell<usize>        [初始值: 1]         │   │
│  │  drop_fn: unsafe fn(*mut u8)                        │   │
│  │  trace_fn: unsafe fn(*const u8, &mut GcVisitor)     │   │
│  │  value: T                                           │   │
│  │    └─ 包含 Weak<T> 指向此 GcBox                     │   │
│  └─────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 分配時序圖

```
┌──────────────┐    ┌──────────────┐    ┌──────────────┐    ┌──────────────┐
│ new_cyclic_  │    │   LocalHeap  │    │   GcBox<T>   │    │   Weak<T>    │
│    weak      │    │              │    │              │    │              │
└──────┬───────┘    └──────┬───────┘    └──────┬───────┘    └──────┬───────┘
       │                   │                   │                   │
       │  alloc::<GcBox>   │                   │                   │
       │──────────────────>│                   │                   │
       │                   │                   │                   │
       │  NonNull<u8>      │                   │                   │
       │<──────────────────│                   │                   │
       │                   │                   │                   │
       │  初始化 metadata (ref_count=1, weak_count=1)              │
       │──────────────────────────────────────>│                   │
       │                   │                   │                   │
       │  建立 Weak<T> 指向 GcBox                                  │
       │──────────────────────────────────────────────────────────>│
       │                   │                   │                   │
       │  呼叫 data_fn(weak)                                       │
       │──────────────────────────────────────────────────────────>│
       │                   │                   │                   │
       │  T (包含 Weak<T>)                                         │
       │<──────────────────────────────────────────────────────────│
       │                   │                   │                   │
       │  寫入 value 到 GcBox                                      │
       │──────────────────────────────────────>│                   │
       │                   │                   │                   │
       │  notify_created_gc()                  │                   │
       │──────────────────────────────────────>│                   │
       │                   │                   │                   │
       │  返回 Gc<T>       │                   │                   │
       │<──────────────────│                   │                   │
       │                   │                   │                   │
```

### 3.3 完整實作程式碼

```rust
// 檔案: crates/rudo-gc/src/ptr.rs

impl<T: Trace> Gc<T> {
    /// Create a self-referential garbage-collected value using a Weak reference.
    ///
    /// The closure receives a `Weak<T>` that will be upgradeable after
    /// construction completes. Store this `Weak` in the constructed value
    /// and call `upgrade()` when access to the self-reference is needed.
    ///
    /// # Panics
    ///
    /// Panics if `T` is a zero-sized type (ZST). ZSTs cannot have meaningful
    /// self-references since they don't occupy memory.
    ///
    /// # Memory Safety
    ///
    /// The `Weak<T>` passed to the closure is valid but points to uninitialized
    /// memory. It must NOT be upgraded within the closure. After `new_cyclic_weak`
    /// returns, the `Weak<T>` stored in the value becomes fully functional.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// use rudo_gc::{Gc, Weak, Trace, GcCell};
    ///
    /// #[derive(Trace)]
    /// struct Node {
    ///     self_ref: GcCell<Option<Weak<Node>>>,
    ///     data: i32,
    /// }
    ///
    /// let node = Gc::new_cyclic_weak(|weak_self| {
    ///     Node {
    ///         self_ref: GcCell::new(Some(weak_self)),
    ///         data: 42,
    ///     }
    /// });
    ///
    /// // Access self through upgrade()
    /// let weak = node.self_ref.borrow();
    /// let self_ref = weak.as_ref().unwrap().upgrade().unwrap();
    /// assert_eq!(self_ref.data, 42);
    /// ```
    pub fn new_cyclic_weak<F>(data_fn: F) -> Self
    where
        F: FnOnce(Weak<T>) -> T,
    {
        // ZSTs cannot have meaningful self-references
        assert!(
            std::mem::size_of::<T>() != 0,
            "Gc::new_cyclic_weak does not support zero-sized types"
        );

        // Step 1: Allocate raw memory for GcBox<T>
        let raw_ptr = with_heap(LocalHeap::alloc::<GcBox<T>>);
        let gc_box = raw_ptr.as_ptr().cast::<GcBox<T>>();

        // Step 2: Partially initialize the GcBox metadata
        // SAFETY: We just allocated this memory, it's valid but uninitialized
        unsafe {
            // Initialize ref_count to 1 (for the Gc we'll return)
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).ref_count),
                Cell::new(NonZeroUsize::MIN),
            );
            
            // Initialize weak_count to 1 (for the Weak we're about to create)
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).weak_count),
                Cell::new(1),
            );
            
            // Initialize function pointers
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).drop_fn),
                GcBox::<T>::drop_fn_for,
            );
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).trace_fn),
                GcBox::<T>::trace_fn_for,
            );
        }

        // Step 3: Create the Weak<T> to pass to the closure
        // SAFETY: gc_box points to valid (partially initialized) memory
        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };
        let weak_self = Weak {
            ptr: Cell::new(Nullable::new(gc_box_ptr)),
            _marker: PhantomData,
        };

        // Step 4: Call the user's closure to construct the value
        // Note: The closure must NOT upgrade the Weak during execution
        let value = data_fn(weak_self);

        // Step 5: Write the value into the GcBox
        // SAFETY: Memory is allocated and metadata is initialized
        unsafe {
            std::ptr::write(std::ptr::addr_of_mut!((*gc_box).value), value);
        }

        // Step 6: Notify the GC about the new allocation
        crate::gc::notify_created_gc();

        // Step 7: Create and return the live Gc<T>
        Self {
            ptr: Cell::new(Nullable::new(gc_box_ptr)),
            _marker: PhantomData,
        }
    }
}
```

---

## 4. 安全性分析

### 4.1 記憶體安全

| 風險 | 緩解措施 |
|------|----------|
| 閉包內 upgrade Weak | Weak 指向未初始化 value，`is_value_dead()` 返回 false，upgrade 成功但存取未定義行為 |
| GcBox 部分初始化 | 使用 `std::ptr::write` 逐欄位初始化，避免 drop 未初始化記憶體 |
| ZST 問題 | 顯式 assert 禁止 ZST |
| 閉包 panic | value 不會被寫入，但 GcBox 記憶體已分配。依賴 GC 清理 |

### 4.2 閉包內升級的風險

**問題場景：**

```rust
Gc::new_cyclic_weak(|weak| {
    let bad = weak.upgrade().unwrap(); // 危險！
    Node { self_ref: GcCell::new(Some(weak)), data: bad.data }
})
```

**分析：**

在 `data_fn` 執行時，`GcBox` 的 `value` 欄位尚未初始化。如果用戶在閉包內呼叫 `upgrade()`：

1. `weak.ptr` 不是 null，指向有效的 `GcBox`
2. `is_value_dead()` 檢查 `weak_count` 的最高位，此時為 0（活著）
3. `upgrade()` 成功，返回 `Some(Gc<T>)`
4. 存取 `Gc::data` 讀取未初始化記憶體 → **未定義行為**

**緩解選項：**

| 選項 | 優點 | 缺點 |
|------|------|------|
| A. 文檔警告 | 無執行時開銷 | 依賴用戶遵守 |
| B. 設置特殊標記 | 可在 upgrade 時檢查 | 增加 Weak 複雜度 |
| C. 延遲設置 weak_count | upgrade 前 weak_count=0 失敗 | 需修改 Weak 邏輯 |

**推薦：選項 A + B 的混合**

```rust
// 在 GcBox 中增加「構建中」標記
impl<T: Trace + ?Sized> GcBox<T> {
    const UNDER_CONSTRUCTION_FLAG: usize = 1 << (usize::BITS - 2);
    
    fn is_under_construction(&self) -> bool {
        (self.weak_count.get() & Self::UNDER_CONSTRUCTION_FLAG) != 0
    }
    
    fn set_under_construction(&self, flag: bool) {
        let current = self.weak_count.get();
        let mask = Self::UNDER_CONSTRUCTION_FLAG;
        if flag {
            self.weak_count.set(current | mask);
        } else {
            self.weak_count.set(current & !mask);
        }
    }
}

// 修改 Weak::upgrade
impl<T: Trace + ?Sized> Weak<T> {
    pub fn upgrade(&self) -> Option<Gc<T>> {
        let ptr = self.ptr.get().as_option()?;
        
        unsafe {
            // 檢查是否在構建中
            if (*ptr.as_ptr()).is_under_construction() {
                panic!("Cannot upgrade Weak during Gc::new_cyclic_weak construction");
            }
            
            // 檢查值是否已 dead
            if (*ptr.as_ptr()).is_value_dead() {
                return None;
            }
            
            (*ptr.as_ptr()).inc_ref();
            crate::gc::notify_created_gc();
            
            Some(Gc {
                ptr: Cell::new(Nullable::new(ptr)),
                _marker: PhantomData,
            })
        }
    }
}
```

### 4.3 Panic 安全

如果 `data_fn` panic：

1. `value` 不會被寫入 `GcBox`
2. Rust 的 unwinding 會執行 stack 上物件的 drop
3. 傳遞給閉包的 `Weak` 會被 drop，減少 `weak_count`
4. `GcBox` 記憶體保持分配狀態（`ref_count=1`, `weak_count=0`）
5. 下次 GC 時，因為沒有任何根引用，會被清理

**但有問題**：`GcBox` 的 `value` 欄位未初始化，GC 嘗試 trace 或 drop 它會導致未定義行為。

**解決方案：使用 Drop Guard**

```rust
pub fn new_cyclic_weak<F>(data_fn: F) -> Self
where
    F: FnOnce(Weak<T>) -> T,
{
    // ... allocation code ...
    
    // Drop guard to clean up on panic
    struct DropGuard {
        ptr: NonNull<u8>,
        completed: bool,
    }
    
    impl Drop for DropGuard {
        fn drop(&mut self) {
            if !self.completed {
                // Panic occurred - deallocate the raw memory
                // SAFETY: Memory was allocated by LocalHeap::alloc
                with_heap(|heap| unsafe {
                    heap.dealloc(self.ptr);
                });
            }
        }
    }
    
    let guard = DropGuard {
        ptr: raw_ptr,
        completed: false,
    };
    
    // ... create weak, call data_fn ...
    
    let value = data_fn(weak_self);
    
    // ... write value ...
    
    // Mark as completed before returning
    std::mem::forget(guard); // Or set guard.completed = true
    
    // ... return Gc ...
}
```

---

## 5. 測試計劃

### 5.1 單元測試

#### 5.1.1 基本功能

```rust
#[test]
fn test_new_cyclic_weak_basic() {
    #[derive(Trace)]
    struct Node {
        self_ref: GcCell<Option<Weak<Node>>>,
        data: i32,
    }
    
    let node = Gc::new_cyclic_weak(|weak| Node {
        self_ref: GcCell::new(Some(weak)),
        data: 42,
    });
    
    // 驗證 data 正確
    assert_eq!(node.data, 42);
    
    // 驗證自引用可升級
    let weak = node.self_ref.borrow();
    let weak = weak.as_ref().expect("Weak should exist");
    let upgraded = weak.upgrade().expect("Upgrade should succeed");
    
    // 驗證是同一個物件
    assert!(Gc::ptr_eq(&node, &upgraded));
}
```

#### 5.1.2 多重自引用

```rust
#[test]
fn test_new_cyclic_weak_multiple_refs() {
    #[derive(Trace)]
    struct MultiRef {
        ref1: GcCell<Option<Weak<MultiRef>>>,
        ref2: GcCell<Option<Weak<MultiRef>>>,
        id: u32,
    }
    
    let obj = Gc::new_cyclic_weak(|weak| MultiRef {
        ref1: GcCell::new(Some(weak.clone())),
        ref2: GcCell::new(Some(weak)),
        id: 123,
    });
    
    let r1 = obj.ref1.borrow().as_ref().unwrap().upgrade().unwrap();
    let r2 = obj.ref2.borrow().as_ref().unwrap().upgrade().unwrap();
    
    assert!(Gc::ptr_eq(&obj, &r1));
    assert!(Gc::ptr_eq(&obj, &r2));
    assert_eq!(obj.id, 123);
}
```

#### 5.1.3 與 GC 互動

```rust
#[test]
fn test_new_cyclic_weak_gc_collection() {
    #[derive(Trace)]
    struct Node {
        self_ref: GcCell<Option<Weak<Node>>>,
    }
    
    let external_weak: Weak<Node>;
    
    {
        let node = Gc::new_cyclic_weak(|weak| Node {
            self_ref: GcCell::new(Some(weak.clone())),
        });
        external_weak = Gc::downgrade(&node);
        
        // node 在此處仍活著
        assert!(external_weak.upgrade().is_some());
    }
    
    // node 離開作用域，觸發 GC
    crate::collect();
    
    // 現在應該無法升級
    assert!(external_weak.upgrade().is_none());
}
```

#### 5.1.4 ZST 拒絕

```rust
#[test]
#[should_panic(expected = "does not support zero-sized types")]
fn test_new_cyclic_weak_zst_panics() {
    #[derive(Trace)]
    struct ZST;
    
    let _ = Gc::new_cyclic_weak(|_weak: Weak<ZST>| ZST);
}
```

#### 5.1.5 閉包內升級拒絕（如果實作選項 B）

```rust
#[test]
#[should_panic(expected = "Cannot upgrade Weak during")]
fn test_new_cyclic_weak_upgrade_in_closure_panics() {
    #[derive(Trace)]
    struct Node {
        data: i32,
    }
    
    let _ = Gc::new_cyclic_weak(|weak: Weak<Node>| {
        let _ = weak.upgrade(); // 應該 panic
        Node { data: 42 }
    });
}
```

### 5.2 整合測試

#### 5.2.1 雙向連結串列

```rust
#[test]
fn test_doubly_linked_list() {
    #[derive(Trace)]
    struct DLNode {
        prev: GcCell<Option<Weak<DLNode>>>,
        next: GcCell<Option<Gc<DLNode>>>,
        value: i32,
    }
    
    // 建立 head
    let head = Gc::new(DLNode {
        prev: GcCell::new(None),
        next: GcCell::new(None),
        value: 0,
    });
    
    // 建立 tail 並連結
    let tail = Gc::new(DLNode {
        prev: GcCell::new(Some(Gc::downgrade(&head))),
        next: GcCell::new(None),
        value: 1,
    });
    
    head.next.borrow_mut().replace(tail.clone());
    
    // 驗證雙向連結
    assert_eq!(head.next.borrow().as_ref().unwrap().value, 1);
    assert_eq!(
        tail.prev.borrow().as_ref().unwrap().upgrade().unwrap().value,
        0
    );
}
```

#### 5.2.2 樹狀結構（父子關係）

```rust
#[test]
fn test_tree_parent_child() {
    #[derive(Trace)]
    struct TreeNode {
        parent: GcCell<Option<Weak<TreeNode>>>,
        children: GcCell<Vec<Gc<TreeNode>>>,
        name: String,
    }
    
    impl TreeNode {
        fn new_root(name: &str) -> Gc<Self> {
            Gc::new(TreeNode {
                parent: GcCell::new(None),
                children: GcCell::new(Vec::new()),
                name: name.to_string(),
            })
        }
        
        fn add_child(parent: &Gc<Self>, name: &str) -> Gc<Self> {
            let child = Gc::new(TreeNode {
                parent: GcCell::new(Some(Gc::downgrade(parent))),
                children: GcCell::new(Vec::new()),
                name: name.to_string(),
            });
            parent.children.borrow_mut().push(child.clone());
            child
        }
    }
    
    let root = TreeNode::new_root("root");
    let child1 = TreeNode::add_child(&root, "child1");
    let child2 = TreeNode::add_child(&root, "child2");
    let grandchild = TreeNode::add_child(&child1, "grandchild");
    
    // 驗證結構
    assert_eq!(root.children.borrow().len(), 2);
    assert_eq!(child1.children.borrow().len(), 1);
    assert_eq!(
        grandchild.parent.borrow().as_ref().unwrap().upgrade().unwrap().name,
        "child1"
    );
}
```

### 5.3 Miri 測試

```rust
// 使用 cargo miri test 執行以下測試

#[test]
fn test_new_cyclic_weak_miri() {
    #[derive(Trace)]
    struct Node {
        self_ref: GcCell<Option<Weak<Node>>>,
    }
    
    let node = Gc::new_cyclic_weak(|weak| Node {
        self_ref: GcCell::new(Some(weak)),
    });
    
    drop(node);
    crate::collect();
}
```

---

## 6. 現有程式碼修改清單

### 6.1 `crates/rudo-gc/src/ptr.rs`

| 行號範圍 | 修改類型 | 說明 |
|----------|----------|------|
| 363-434 | 修改 | 標記 `new_cyclic` 為 deprecated |
| 435 後 | 新增 | 插入 `new_cyclic_weak` 實作 |
| 98-101（如採用選項B） | 修改 | 增加 `UNDER_CONSTRUCTION_FLAG` 常數 |
| 750-772（如採用選項B） | 修改 | `Weak::upgrade` 增加構建中檢查 |

### 6.2 `crates/rudo-gc/src/lib.rs`

| 行號 | 修改類型 | 說明 |
|------|----------|------|
| 匯出區 | 確認 | 確保 `Weak` 已公開匯出 |

### 6.3 `crates/rudo-gc/src/heap.rs`（如需 panic 保護）

| 修改類型 | 說明 |
|----------|------|
| 新增 | `LocalHeap::dealloc` 方法用於 panic 清理 |

---

## 7. 文檔更新

### 7.1 API 文檔

- `Gc::new_cyclic_weak` 的完整 rustdoc
- `Gc::new_cyclic` 標記 `#[deprecated]` 並說明替代方案
- 模組級文檔增加自引用結構的使用指南

### 7.2 範例更新

```rust
// crates/rudo-gc/examples/cyclic_ref.rs

//! Example: Creating self-referential structures with Gc

use rudo_gc::{Gc, Weak, Trace, GcCell, collect};

#[derive(Trace)]
struct Graph {
    nodes: Vec<Gc<Node>>,
}

#[derive(Trace)]
struct Node {
    self_ref: GcCell<Option<Weak<Node>>>,
    neighbors: GcCell<Vec<Weak<Node>>>,
    id: usize,
}

fn main() {
    // 建立自引用節點
    let node = Gc::new_cyclic_weak(|weak| Node {
        self_ref: GcCell::new(Some(weak)),
        neighbors: GcCell::new(Vec::new()),
        id: 1,
    });
    
    println!("Created node with ID: {}", node.id);
    
    // 透過自引用存取
    if let Some(ref weak) = *node.self_ref.borrow() {
        if let Some(self_ref) = weak.upgrade() {
            println!("Self-reference works! ID: {}", self_ref.id);
        }
    }
    
    drop(node);
    collect();
    println!("Memory cleaned up successfully");
}
```

---

## 8. 效能考量

### 8.1 額外開銷分析

| 操作 | 額外開銷 |
|------|----------|
| 分配 | 無（與 `Gc::new` 相同） |
| 建構 | 多一次 `Weak` clone（如果閉包 clone） |
| 存取自引用 | `upgrade()` 呼叫（檢查 `is_value_dead`） |
| GC 遍歷 | 無（`Weak` 不在 trace 路徑中） |

### 8.2 與替代方案比較

| 方案 | 分配開銷 | 每次存取開銷 | 記憶體開銷 |
|------|----------|--------------|------------|
| new_cyclic_weak | O(1) | upgrade() 檢查 | 0 |
| Allocation ID | O(1) + 原子操作 | 0 | +8 bytes/object |
| TraceMut | O(n) rehydration | 0 | 0 |

---

## 9. 里程碑與時程

| 階段 | 任務 | 預估時間 |
|------|------|----------|
| 1 | 實作 `new_cyclic_weak` 基本版本 | 2 小時 |
| 2 | 新增單元測試 | 1 小時 |
| 3 | 實作 `UNDER_CONSTRUCTION_FLAG` 保護 | 1 小時 |
| 4 | 新增整合測試 | 1 小時 |
| 5 | 標記 `new_cyclic` deprecated | 30 分鐘 |
| 6 | 更新文檔和範例 | 1 小時 |
| 7 | 執行 Miri 測試 | 30 分鐘 |
| **總計** | | **約 7 小時** |

---

## 10. 風險評估

| 風險 | 可能性 | 影響 | 緩解措施 |
|------|--------|------|----------|
| 閉包內升級導致 UB | 中 | 高 | 實作 `UNDER_CONSTRUCTION_FLAG` |
| Panic 導致記憶體洩漏 | 低 | 中 | 實作 Drop Guard |
| 用戶誤用 deprecated `new_cyclic` | 中 | 中 | 明確 deprecation 警告 |
| 與現有程式碼不相容 | 低 | 低 | 純新增 API，不破壞現有功能 |

---

## 11. 審核檢查清單

- [ ] 程式碼通過 `cargo clippy --all-targets -- -D warnings`
- [ ] 程式碼通過 `cargo fmt --check`
- [ ] 所有單元測試通過
- [ ] Miri 測試通過（`cargo miri test`）
- [ ] API 文檔完整
- [ ] `CHANGELOG.md` 更新
- [ ] `new_cyclic` 正確標記 deprecated

---

*技術規格書結束*
