# `new_cyclic` 完整支援實作建議

**日期：** 2026-01-14  
**作者：** R. Kent Dybvig & John McCarthy（平行世界協作）  
**範疇：** `rudo-gc` 的 `Gc::new_cyclic` 自引用循環支援

---

## 摘要

本文件分析 `rudo-gc` 中 `Gc::new_cyclic` 的現有限制，並提供三種完整的實作方案。我們從 Chez Scheme 的經驗和 Lisp 的 GC 理論出發，為 Rust 的型別系統約束提供實用的解決策略。

---

## 1. 問題分析

### 1.1 現有程式碼的限制

當前 `new_cyclic` 實作位於 `crates/rudo-gc/src/ptr.rs:393-434`：

```rust
pub fn new_cyclic<F: FnOnce(Self) -> T>(data_fn: F) -> Self {
    // Allocate space
    let ptr = with_heap(LocalHeap::alloc::<GcBox<T>>);
    let gc_box = ptr.as_ptr().cast::<GcBox<T>>();

    // Create a dead Gc to pass to the closure
    let dead_gc = Self {
        ptr: Cell::new(Nullable::new(unsafe { NonNull::new_unchecked(gc_box) }).as_null()),
        _marker: PhantomData,
    };

    // Call the closure to get the value
    let value = data_fn(dead_gc);

    // Initialize the GcBox
    unsafe {
        gc_box.write(GcBox {
            ref_count: Cell::new(NonZeroUsize::MIN),
            weak_count: Cell::new(0),
            drop_fn: GcBox::<T>::drop_fn_for,
            trace_fn: GcBox::<T>::trace_fn_for,
            value,
        });
    }

    let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

    // Create the live Gc
    let gc = Self {
        ptr: Cell::new(Nullable::new(gc_box_ptr)),
        _marker: PhantomData,
    };

    // Rehydrate any dead Gcs in the value that point to us
    unsafe {
        rehydrate_self_refs(gc_box_ptr, &(*gc_box).value);
    }

    gc
}
```

### 1.2 核心問題

**問題：`rehydrate_self_refs` 無法完成其工作。**

`rehydrate_self_refs` 函數（第 897-923 行）使用 `Trace` trait 遍歷物件圖，但：

1. **`Trace::trace` 是唯讀的**：trait 簽名是 `fn trace(&self, visitor: &mut impl Visitor)`，只提供 `&self`，無法修改 `Gc<T>` 內部的指標。

2. **型別抹除問題**：`Visitor::visit` 接收 `&Gc<U>` 其中 `U` 是任意型別，我們無法確定它是否指向我們正在構建的物件。

3. **Rust 參考語義**：即使我們可以識別正確的 `Gc`，透過 `&` 修改 `Cell<Nullable<GcBox<T>>>` 也不違反 Rust 規則，但 `Trace` trait 的設計假設是檢視而非修改。

### 1.3 John McCarthy 的觀察

> *「在 Lisp 中，我們有 cons cell 的明確結構——每個 cell 就是兩個指標。自引用結構透過 `rplaca` 和 `rplacd` 設置回指是自然的。Rust 的所有權系統要求我們在構建期間就確定所有權關係，這與我 1960 年的 `reclaim()` 設計有根本差異。」*

### 1.4 R. Kent Dybvig 的觀察

> *「Chez Scheme 解決這個問題的方式是：我們不需要 `new_cyclic`。Scheme 的 box 是可變的，用戶構建物件後再填入自引用。然而，對於 Rust 的 immutable-by-default 哲學，我們需要一個在構建時就能安全建立循環的機制。關鍵是：**儲存足夠的身份資訊來識別 'dead' 指標**。」*

---

## 2. 解決方案

我們提供三種方案，各有取捨：

| 方案 | 複雜度 | 效能影響 | API 變更 | 推薦程度 |
|------|--------|----------|----------|----------|
| A. WeakGc 中繼 | 低 | 最小 | 新型別 | ⭐⭐⭐⭐⭐ |
| B. Allocation ID | 中 | 每物件 +8 bytes | 無 | ⭐⭐⭐⭐ |
| C. TraceMut trait | 高 | 無 | 新 unsafe trait | ⭐⭐⭐ |

---

## 方案 A：WeakGc 中繼（推薦）

### 設計理念

利用現有的 `Weak<T>` 機制，傳遞一個 `Weak` 指標給閉包，構建完成後用戶透過 `upgrade()` 取得強引用。

### 新 API

```rust
impl<T: Trace> Gc<T> {
    /// Create a self-referential garbage-collected value using a Weak reference.
    ///
    /// The closure receives a `Weak<T>` that will be upgradeable after
    /// construction completes. The caller must store this Weak in the
    /// constructed value and call `upgrade()` when needed.
    ///
    /// # Examples
    ///
    /// ```rust
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
    /// if let Some(weak) = &*node.self_ref.borrow() {
    ///     if let Some(self_ref) = weak.upgrade() {
    ///         assert_eq!(self_ref.data, 42);
    ///     }
    /// }
    /// ```
    pub fn new_cyclic_weak<F: FnOnce(Weak<T>) -> T>(data_fn: F) -> Self {
        // Handle Zero-Sized Types
        if std::mem::size_of::<T>() == 0 {
            // ZSTs cannot have meaningful self-references
            // Create a dangling weak and proceed
            let dangling_weak = Weak::default();
            let value = data_fn(dangling_weak);
            return Self::new_zst(value);
        }

        // Allocate space
        let ptr = with_heap(LocalHeap::alloc::<GcBox<T>>);
        let gc_box = ptr.as_ptr().cast::<GcBox<T>>();
        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

        // Initialize weak_count to 1 (for the Weak we're about to create)
        // We need to partially initialize the GcBox to make the Weak valid
        unsafe {
            // Initialize only the metadata fields first
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).ref_count),
                Cell::new(NonZeroUsize::MIN),
            );
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).weak_count),
                Cell::new(1), // One weak reference about to be created
            );
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).drop_fn),
                GcBox::<T>::drop_fn_for,
            );
            std::ptr::write(
                std::ptr::addr_of_mut!((*gc_box).trace_fn),
                GcBox::<T>::trace_fn_for,
            );
        }

        // Create the Weak reference to pass to the closure
        let weak_self = Weak {
            ptr: Cell::new(Nullable::new(gc_box_ptr)),
            _marker: PhantomData,
        };

        // Call the closure to get the value
        let value = data_fn(weak_self);

        // Now write the value field
        // SAFETY: We just allocated this memory and initialized metadata
        unsafe {
            std::ptr::write(std::ptr::addr_of_mut!((*gc_box).value), value);
        }

        // Notify that we created a Gc
        crate::gc::notify_created_gc();

        Self {
            ptr: Cell::new(Nullable::new(gc_box_ptr)),
            _marker: PhantomData,
        }
    }
}
```

### 優點

1. **使用現有機制**：`Weak<T>` 已經完整實作且經過測試。
2. **型別安全**：沒有型別抹除問題，`Weak<T>` 知道它指向 `T`。
3. **無執行時開銷**：不需要額外的識別資訊。
4. **語義清晰**：用戶明確知道自引用是「弱」的。

### 缺點

1. **API 變更**：用戶需使用 `Weak<T>` 而非 `Gc<T>` 儲存自引用。
2. **升級開銷**：每次存取自引用需要 `upgrade()` 呼叫。
3. **可能失敗**：如果外部全部 drop，`upgrade()` 會返回 `None`。

### 使用模式

```rust
#[derive(Trace)]
struct TreeNode {
    parent: GcCell<Option<Weak<TreeNode>>>,
    children: GcCell<Vec<Gc<TreeNode>>>,
    data: i32,
}

impl TreeNode {
    fn new_root(data: i32) -> Gc<Self> {
        Gc::new(TreeNode {
            parent: GcCell::new(None),
            children: GcCell::new(Vec::new()),
            data,
        })
    }

    fn new_with_parent(data: i32, parent: &Gc<TreeNode>) -> Gc<Self> {
        let child = Gc::new(TreeNode {
            parent: GcCell::new(Some(Gc::downgrade(parent))),
            children: GcCell::new(Vec::new()),
            data,
        });
        parent.children.borrow_mut().push(child.clone());
        child
    }
}
```

---

## 方案 B：Allocation ID

### 設計理念

為每個 `GcBox` 分配一個唯一的 ID，在 rehydration 時比較 ID 而非指標。

### GcBox 修改

```rust
use std::sync::atomic::{AtomicU64, Ordering};

/// Global counter for allocation IDs
static NEXT_ALLOC_ID: AtomicU64 = AtomicU64::new(1);

#[repr(C)]
pub struct GcBox<T: Trace + ?Sized> {
    /// Unique allocation identifier for cyclic reference resolution
    alloc_id: u64,
    /// Current reference count
    ref_count: Cell<NonZeroUsize>,
    /// Number of weak references
    weak_count: Cell<usize>,
    /// Type-erased destructor
    pub(crate) drop_fn: unsafe fn(*mut u8),
    /// Type-erased trace function
    pub(crate) trace_fn: unsafe fn(*const u8, &mut GcVisitor),
    /// The user's data
    value: T,
}

impl<T: Trace> GcBox<T> {
    fn new_id() -> u64 {
        NEXT_ALLOC_ID.fetch_add(1, Ordering::Relaxed)
    }
}
```

### Dead Gc 修改

```rust
/// A "dead" Gc stores the allocation ID instead of the actual pointer.
/// The pointer field uses address 0 but preserves the allocation ID
/// in a separate field accessed during rehydration.
impl<T: Trace> Gc<T> {
    /// Create a dead Gc that stores an allocation ID for rehydration.
    fn new_dead_with_id(alloc_id: u64) -> Self {
        // Store the allocation ID in thread-local storage for rehydration
        PENDING_ALLOC_ID.with(|cell| cell.set(Some(alloc_id)));
        
        Self {
            ptr: Cell::new(Nullable::null()),
            _marker: PhantomData,
        }
    }
    
    /// Get the pending allocation ID if this is a dead Gc created by new_cyclic.
    fn pending_alloc_id(&self) -> Option<u64> {
        if self.ptr.get().is_null() {
            PENDING_ALLOC_ID.with(|cell| cell.get())
        } else {
            None
        }
    }
}

thread_local! {
    static PENDING_ALLOC_ID: Cell<Option<u64>> = Cell::new(None);
}
```

### 新的 rehydrate_self_refs

```rust
/// Rehydrate dead self-references using allocation ID matching.
fn rehydrate_self_refs<T: Trace + ?Sized>(target: NonNull<GcBox<T>>, value: &T) {
    let target_id = unsafe { (*target.as_ptr()).alloc_id };
    
    struct Rehydrator {
        target_id: u64,
        target_ptr_raw: *mut u8,
    }

    impl Visitor for Rehydrator {
        fn visit<U: Trace + ?Sized>(&mut self, gc: &Gc<U>) {
            // Check if this is a dead Gc with matching allocation ID
            if gc.ptr.get().is_null() {
                // We need to check if this dead Gc was created for our target
                // This requires the TraceMut approach or a side table
                
                // For now, we use type-based heuristics:
                // If the type U is the same as our target type, rehydrate.
                // This requires TypeId comparison at runtime.
                
                // Note: This is where the limitation exists.
                // We cannot safely rehydrate without additional type information.
            }
        }
    }

    let mut rehydrator = Rehydrator {
        target_id,
        target_ptr_raw: target.as_ptr().cast(),
    };
    value.trace(&mut rehydrator);
}
```

### 問題

即使有 allocation ID，我們仍然無法透過 `&Gc<U>` 修改 `Gc<U>` 的內部指標。這需要方案 C 的 `TraceMut` 來解決。

### 結論

**Allocation ID 是方案 C 的前置條件，但單獨使用不足以解決問題。**

---

## 方案 C：TraceMut Trait

### 設計理念

引入一個新的 `TraceMut` trait，允許在遍歷時修改 `Gc` 指標。

### 新 Trait 定義

```rust
/// A type that can be traced and modified by the garbage collector.
///
/// # Safety
///
/// Implementations **MUST** correctly visit all `Gc<T>` fields by calling
/// `visitor.visit_mut()` on each one. This is similar to `Trace`, but
/// allows the visitor to modify the Gc pointer.
///
/// **WARNING:** Incorrect implementations can cause memory corruption.
pub unsafe trait TraceMut: Trace {
    /// Visit all `Gc` pointers contained within this value, allowing mutation.
    fn trace_mut(&mut self, visitor: &mut impl VisitorMut);
}

/// A visitor that can modify Gc pointers during traversal.
pub trait VisitorMut: Visitor {
    /// Visit a garbage-collected pointer, potentially modifying it.
    fn visit_mut<T: Trace + ?Sized>(&mut self, gc: &mut Gc<T>);
}
```

### Derive Macro 擴展

```rust
// 用戶程式碼
#[derive(Trace, TraceMut)]
struct Node {
    self_ref: Gc<Node>,
    data: i32,
}

// 展開後
unsafe impl Trace for Node {
    fn trace(&self, visitor: &mut impl Visitor) {
        visitor.visit(&self.self_ref);
    }
}

unsafe impl TraceMut for Node {
    fn trace_mut(&mut self, visitor: &mut impl VisitorMut) {
        visitor.visit_mut(&mut self.self_ref);
    }
}
```

### 完整 new_cyclic 實作

```rust
impl<T: Trace + TraceMut> Gc<T> {
    /// Create a self-referential garbage-collected value.
    ///
    /// The closure receives a "dead" `Gc` that will be rehydrated after
    /// construction completes.
    ///
    /// # Requirements
    ///
    /// The type `T` must implement `TraceMut` to enable rehydration.
    ///
    /// # Examples
    ///
    /// ```rust
    /// use rudo_gc::{Gc, Trace, TraceMut};
    ///
    /// #[derive(Trace, TraceMut)]
    /// struct Node {
    ///     self_ref: Gc<Node>,
    ///     data: i32,
    /// }
    ///
    /// let node = Gc::new_cyclic(|this| Node {
    ///     self_ref: this,
    ///     data: 42,
    /// });
    ///
    /// // The self_ref is now a live Gc pointing to node itself
    /// assert_eq!(node.self_ref.data, 42);
    /// ```
    pub fn new_cyclic<F: FnOnce(Self) -> T>(data_fn: F) -> Self {
        if std::mem::size_of::<T>() == 0 {
            panic!("new_cyclic does not support zero-sized types");
        }

        // Generate unique allocation ID
        let alloc_id = GcBox::<T>::new_id();

        // Allocate space
        let ptr = with_heap(LocalHeap::alloc::<GcBox<T>>);
        let gc_box = ptr.as_ptr().cast::<GcBox<T>>();

        // Create a dead Gc with the allocation ID
        let dead_gc = Self {
            ptr: Cell::new(Nullable::null()),
            _marker: PhantomData,
        };

        // Store the allocation ID in thread-local for rehydration
        CYCLIC_CONTEXT.with(|ctx| {
            ctx.set(Some(CyclicContext {
                alloc_id,
                target_ptr: gc_box.cast(),
            }));
        });

        // Call the closure to get the value
        let value = data_fn(dead_gc);

        // Initialize the GcBox
        unsafe {
            gc_box.write(GcBox {
                alloc_id,
                ref_count: Cell::new(NonZeroUsize::MIN),
                weak_count: Cell::new(0),
                drop_fn: GcBox::<T>::drop_fn_for,
                trace_fn: GcBox::<T>::trace_fn_for,
                value,
            });
        }

        let gc_box_ptr = unsafe { NonNull::new_unchecked(gc_box) };

        // Create the live Gc
        let gc = Self {
            ptr: Cell::new(Nullable::new(gc_box_ptr)),
            _marker: PhantomData,
        };

        // Rehydrate dead Gcs using TraceMut
        unsafe {
            rehydrate_with_trace_mut(gc_box_ptr);
        }

        // Clear the cyclic context
        CYCLIC_CONTEXT.with(|ctx| ctx.set(None));

        // Notify that we created a Gc
        crate::gc::notify_created_gc();

        gc
    }
}

/// Context for cyclic reference rehydration
struct CyclicContext {
    alloc_id: u64,
    target_ptr: *mut u8,
}

thread_local! {
    static CYCLIC_CONTEXT: Cell<Option<CyclicContext>> = Cell::new(None);
}

/// Rehydrate dead references using TraceMut
unsafe fn rehydrate_with_trace_mut<T: Trace + TraceMut>(target: NonNull<GcBox<T>>) {
    struct Rehydrator;

    impl Visitor for Rehydrator {
        fn visit<U: Trace + ?Sized>(&mut self, _gc: &Gc<U>) {
            // Read-only visit, do nothing
        }
    }

    impl VisitorMut for Rehydrator {
        fn visit_mut<U: Trace + ?Sized>(&mut self, gc: &mut Gc<U>) {
            if gc.ptr.get().is_null() {
                // This is a dead Gc - check if it matches our context
                CYCLIC_CONTEXT.with(|ctx| {
                    if let Some(context) = ctx.get() {
                        // Rehydrate by setting the pointer
                        // SAFETY: We're inside new_cyclic and the GcBox is now initialized
                        gc.ptr.set(Nullable::from_ptr(context.target_ptr.cast()));
                    }
                });
            }
        }
    }

    let mut rehydrator = Rehydrator;
    (*target.as_ptr()).value.trace_mut(&mut rehydrator);
}
```

### 優點

1. **保留原始 API**：用戶使用 `Gc<T>` 儲存自引用，語義自然。
2. **型別安全**：在編譯期確保 `TraceMut` 被正確實作。
3. **精確控制**：可以僅對需要的欄位啟用修改。

### 缺點

1. **新的 unsafe trait**：增加使用者需要理解的概念。
2. **Derive macro 擴展**：需要修改 `rudo-gc-derive` crate。
3. **程式碼膨脹**：每個型別多一個 `trace_mut` 方法。

---

## 3. 推薦實作順序

### 階段 1：實作方案 A（`new_cyclic_weak`）

這是最低風險、最快實現的方案。可以立即提供可用的自引用功能。

**修改清單：**
1. `ptr.rs`: 新增 `Gc::new_cyclic_weak` 方法
2. `lib.rs`: 匯出新方法
3. 更新文檔和範例

**預估工作量：** 2-4 小時

### 階段 2：保留現有 `new_cyclic` 作為 deprecated

```rust
#[deprecated(
    since = "0.2.0",
    note = "Use new_cyclic_weak instead. new_cyclic does not work correctly."
)]
pub fn new_cyclic<F: FnOnce(Self) -> T>(data_fn: F) -> Self {
    // ... 現有實作保留，加上 warning
    eprintln!("WARNING: Gc::new_cyclic does not properly rehydrate self-references");
    // ...
}
```

### 階段 3（可選）：實作方案 C

如果用戶強烈需要 `Gc<T>` 形式的自引用，可在後續版本實作 `TraceMut`。

**修改清單：**
1. `trace.rs`: 新增 `TraceMut` 和 `VisitorMut` traits
2. `ptr.rs`: 新增 `GcBox::alloc_id`，修改 `new_cyclic`
3. `rudo-gc-derive/src/lib.rs`: 擴展 derive macro 支援 `TraceMut`
4. 更新測試和文檔

**預估工作量：** 1-2 天

---

## 4. Dybvig 與 McCarthy 的額外建議

### 4.1 關於循環的語義（McCarthy）

> *「在 Lisp 中，循環結構透過修改已分配的結構來建立。Rust 的 `new_cyclic` 嘗試在構建期間就建立循環，這是一種『forward declaration』模式。考慮另一種設計：兩步驟構建。」*

```rust
// 兩步驟模式
impl<T: Trace> Gc<T> {
    /// Allocate without initializing, returning a builder.
    pub fn allocate() -> GcBuilder<T> { ... }
}

struct GcBuilder<T: Trace> {
    ptr: NonNull<MaybeUninit<GcBox<T>>>,
}

impl<T: Trace> GcBuilder<T> {
    /// Get a reference to this allocation (for self-references)
    pub fn weak_ref(&self) -> Weak<T> { ... }
    
    /// Finalize the allocation with a value
    pub fn build(self, value: T) -> Gc<T> { ... }
}

// 使用
let builder = Gc::<Node>::allocate();
let weak = builder.weak_ref();
let node = builder.build(Node { self_ref: weak, data: 42 });
```

### 4.2 關於效能（Dybvig）

> *「在 Chez Scheme 中，我們避免在 allocation 路徑上增加任何無條件的工作。Allocation ID 會為每個物件增加 8 bytes 和一個 atomic 操作。如果 `new_cyclic` 不常用，這個開銷可能不值得。方案 A 是正確的權衡：僅在需要時支付成本。」*

### 4.3 關於 GC 的整合（McCarthy）

> *「現有的 `Weak<T>` 實作已經與 GC 的標記/清掃整合。用它來實作 `new_cyclic_weak` 不僅是最簡單的，也是最與現有設計一致的。當 `Gc<T>` 被回收時，`Weak<T>` 的 `upgrade()` 返回 `None`——這正是自引用結構應有的行為。」*

---

## 5. 測試計劃

### 5.1 基本功能測試

```rust
#[test]
fn test_new_cyclic_weak_self_reference() {
    #[derive(Trace)]
    struct Node {
        self_ref: GcCell<Option<Weak<Node>>>,
    }
    
    let node = Gc::new_cyclic_weak(|weak| {
        Node {
            self_ref: GcCell::new(Some(weak)),
        }
    });
    
    let weak = node.self_ref.borrow();
    let weak = weak.as_ref().unwrap();
    let upgraded = weak.upgrade();
    assert!(upgraded.is_some());
    assert!(Gc::ptr_eq(&node, &upgraded.unwrap()));
}

#[test]
fn test_new_cyclic_weak_after_drop() {
    #[derive(Trace)]
    struct Node {
        self_ref: GcCell<Option<Weak<Node>>>,
    }
    
    let weak_external;
    {
        let node = Gc::new_cyclic_weak(|weak| {
            Node {
                self_ref: GcCell::new(Some(weak.clone())),
            }
        });
        weak_external = node.self_ref.borrow().as_ref().unwrap().clone();
    }
    
    collect();
    assert!(weak_external.upgrade().is_none());
}
```

### 5.2 複雜結構測試

```rust
#[test]
fn test_doubly_linked_list() {
    #[derive(Trace)]
    struct DListNode {
        prev: GcCell<Option<Weak<DListNode>>>,
        next: GcCell<Option<Gc<DListNode>>>,
        data: i32,
    }
    
    // 建立三個節點的雙向連結串列
    let node1 = Gc::new(DListNode {
        prev: GcCell::new(None),
        next: GcCell::new(None),
        data: 1,
    });
    
    let node2 = Gc::new(DListNode {
        prev: GcCell::new(Some(Gc::downgrade(&node1))),
        next: GcCell::new(None),
        data: 2,
    });
    
    node1.next.borrow_mut().replace(node2.clone());
    
    // 驗證連結
    assert_eq!(node1.next.borrow().as_ref().unwrap().data, 2);
    assert_eq!(
        node2.prev.borrow().as_ref().unwrap().upgrade().unwrap().data,
        1
    );
}
```

---

## 6. 結論

| 選項 | 推薦 | 理由 |
|------|------|------|
| 方案 A | ✅ **強烈推薦** | 最小變更、最大相容性、利用現有機制 |
| 方案 B | ❌ 不推薦單獨使用 | 僅提供識別，不解決修改問題 |
| 方案 C | ⚠️ 進階選項 | 如果 API 美學重要於簡單性 |

**R. Kent Dybvig 總結：**

> *「在 Chez Scheme 的發展中，我學到最佳的 GC 設計是與語言的語義自然融合的設計。Rust 的 `Weak<T>` 已經表達了『可能無效的參照』這個概念。利用它來實作自引用，不是妥協，而是正確：**自引用本質上就是一種弱參照關係**——物件不應該因為引用自己而永遠存活。」*

**John McCarthy 總結：**

> *「六十年前我寫 `reclaim()` 時，沒想過會有今天這樣複雜的型別系統。但 GC 的核心問題從未改變：安全地識別和回收不再使用的記憶體。`rudo-gc` 團隊選擇 `new_cyclic_weak` 是正確的——它尊重 Rust 的所有權語義，同時提供自引用能力。這是理論與實踐的良好結合。」*

---

*文件結束*
