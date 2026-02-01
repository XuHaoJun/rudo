# HandleScope 技術規格文件 v2

**版本**: 2.1  
**日期**: 2026-02-01  
**作者**: rudo-gc Team  
**狀態**: 草稿  
**變更紀錄**: 基於深度審查修訂 (2026-02-01)

---

## 版本歷史

### v2.1 (2026-02-01) - 深度審查修訂

基於 V8 實現比較和安全性分析，修復以下關鍵設計問題：

**🔴 關鍵修復**:

1. **EscapeableHandleScope::escape 生命週期問題**
   - 問題: 原設計的 `'outer` 參數可以被呼叫者隨意指定
   - 修復: `escape()` 現在需要 `parent: &'parent HandleScope` 參數來約束返回的 Handle 生命週期

2. **HandleScope 使用共享引用**
   - 問題: 使用 `&mut ThreadControlBlock` 導致無法巢狀 scope
   - 修復: 改用 `&ThreadControlBlock`，透過 `UnsafeCell` 實現內部可變性

3. **allocate_slot 別名問題**
   - 問題: 從 `&self` 創建 `&mut LocalHandles` 違反 Rust 別名規則
   - 修復: 完全使用原始指標操作，不創建臨時 `&mut` 引用

4. **AsyncHandleScope 註冊機制**
   - 問題: 使用指標比較會造成自引用結構問題
   - 修復: 改用 ID-based 註冊機制

5. **SealedHandleScope 使用 sealed_level**
   - 問題: 操作 `limit` 的方式可能被 `add_block` 覆蓋
   - 修復: 使用 `sealed_level` 欄位 (V8 設計模式)

6. **AsyncHandle 安全存取**
   - 問題: `unsafe fn get()` 的安全契約無法在運行時驗證
   - 修復: 新增 `scope.with_guard()` 模式提供生命週期綁定的安全存取

**🟡 重要改進**:

7. **ThreadControlBlock 新增方法**
   - `local_handles_ptr()`: 返回原始指標避免別名問題
   - `add_handle_block()`: 分配新 block
   - `remove_unused_blocks()`: 回收未使用的 blocks

8. **LocalHandles 完整實現**
   - `scope_data_ptr()`: 原始指標存取
   - `remove_unused_blocks()`: Block 回收
   - `iterate()`: 精確 GC 根遍歷

9. **current_thread_control_block() 函數**
   - 新增給 `spawn_with_gc!` macro 使用的函數

10. **AsyncHandleGuard 新類型**
    - 提供安全的 AsyncHandle 存取模式
    - 生命週期綁定到 scope 借用

## 變更摘要 (v1 → v2)

| 項目 | v1 設計 | v2 設計 | 理由 |
|------|---------|---------|------|
| Handle 生命週期 | `Handle<T>` 無約束 | `Handle<'scope, T>` 綁定 scope | 防止 Handle 逃逸 |
| Handle 創建 | `Handle::new(&gc)` 隱式 | `scope.handle(&gc)` 顯式 | 避免全域狀態 |
| API 設計 | 隱式 `current()` | 顯式傳遞 scope | Rust explicit philosophy |
| Interior Pointer | 未處理 | 完整支援 | 修復 UAF 漏洞 |
| Escape 機制 | 未定義 | `EscapeableHandleScope` | 跨 scope 傳遞 |
| Async 整合 | `root_guard()` 手動 | `AsyncHandleScope` 自動 | 消除 unsoundness |

---

## 摘要

本文件描述 `rudo-gc` 垃圾收集器的 **HandleScope v2** 實作規格。此版本基於評審回饋，著重於：

1. **編譯期安全保證**: Handle 生命週期綁定至 Scope
2. **API 明確性**: 消除隱式全域狀態
3. **Async 安全**: 提供 first-class async 支援
4. **完整 Interior Pointer 支援**: 修復 UAF 漏洞

---

## 1. 核心設計原則

### 1.1 設計哲學

```
┌─────────────────────────────────────────────────────────────┐
│                    HandleScope v2 設計原則                     │
├─────────────────────────────────────────────────────────────┤
│ 1. Explicit over Implicit                                    │
│    - 不使用 thread-local current scope                        │
│    - Handle 必須顯式從 scope 創建                              │
│                                                              │
│ 2. Compile-time Safety                                       │
│    - Handle<'scope, T> 的生命週期綁定                          │
│    - 無法在 safe Rust 中創建 dangling handle                   │
│                                                              │
│ 3. Zero-cost Abstraction                                     │
│    - Handle 在 release mode 下編譯為單一指標                    │
│    - Scope 管理無需額外 heap allocation                        │
│                                                              │
│ 4. Async-first Design                                        │
│    - AsyncHandleScope 原生支援 async/await                    │
│    - 消除 root_guard() 的 unsoundness                         │
└─────────────────────────────────────────────────────────────┘
```

### 1.2 與 V8 的差異

| 特性 | V8 | rudo-gc v2 | 理由 |
|------|-----|------------|------|
| Handle 類型 | `Handle<T>` 無生命週期 | `Handle<'s, T>` 有生命週期 | Rust 類型系統優勢 |
| Scope 訪問 | `Isolate::GetCurrent()` | 顯式傳遞 | 避免隱式狀態 |
| Escape | `EscapableHandleScope` | `EscapeableHandleScope` | 同 V8 設計 |
| Direct Handle | CSS-dependent | 預設精確追蹤 | 目標 Soundness |

---

## 2. 資料結構定義

### 2.1 HandleScopeData

```rust
// crates/rudo-gc/src/handles/mod.rs

/// HandleScope 的執行時資料
/// 
/// 儲存於 ThreadControlBlock，用於追蹤當前 scope 的 handle 分配狀態。
#[derive(Debug)]
pub struct HandleScopeData {
    /// 下一個可分配 handle 的位置
    next: *mut HandleSlot,
    /// 當前 block 的結尾位置
    limit: *mut HandleSlot,
    /// Nested scope 層數 (用於驗證和除錯)
    level: u32,
    /// Sealed level - handle allocation prohibited at or below this level (debug only)
    #[cfg(debug_assertions)]
    sealed_level: u32,
}

impl HandleScopeData {
    pub const fn new() -> Self {
        Self {
            next: std::ptr::null_mut(),
            limit: std::ptr::null_mut(),
            level: 0,
            #[cfg(debug_assertions)]
            sealed_level: 0,
        }
    }
    
    #[inline]
    pub fn is_active(&self) -> bool {
        self.level > 0
    }
    
    #[cfg(debug_assertions)]
    #[inline]
    pub fn is_sealed(&self) -> bool {
        self.level <= self.sealed_level
    }
}
// ... (skip Default impl) ...

// ... (skip HandleSlot/HandleBlock definitions) ...

// In LocalHandles::iterate
            // 遍歷 block 中的每個 slot
            let mut current = start as *const HandleSlot;
            while current < block_end {
                let slot = unsafe { &*current };
                let gc_box_ptr = slot.as_ptr();
                
                // 標記這個 GcBox
                // 由於 HandleScope 精確追蹤 Handles，且 Handles 總是指向 GcBox 頭部，
                // 我們可以直接標記該指標為 root。
                // 
                // 注意：這裡假設 Handle 創建時已經保證了 gc_box_ptr 的有效性。
                // 如果需要額外安全性，可以使用 find_gc_box_from_ptr 驗證。
                unsafe {
                     visitor.mark_root(NonNull::new_unchecked(gc_box_ptr as *mut GcBox<()>));
                }
                
                current = unsafe { current.add(1) };
            }
            
            current_block = block.next;
        }
    }
}

impl Default for LocalHandles {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LocalHandles {
    fn drop(&mut self) {
        // 釋放所有 blocks
        let mut current = self.blocks;
        while let Some(block_ptr) = current {
            let block = unsafe { Box::from_raw(block_ptr.as_ptr()) };
            current = block.next;
        }
    }
}
```

---

## 3. HandleScope 實作

### 3.1 基本 HandleScope

```rust
/// HandleScope - RAII 風格的 handle 管理
/// 
/// HandleScope 定義了 handles 的有效範圍。當 scope 結束時，
/// 所有在該 scope 內創建的 handles 都會自動失效。
/// 
/// # 生命週期
/// 
/// `'env` 代表 scope 所屬的執行環境（通常是 ThreadControlBlock）。
/// 所有 Handle 都會綁定到 HandleScope 自身的生命週期。
/// 
/// # 設計決策：使用共享引用
/// 
/// HandleScope 使用 `&ThreadControlBlock` 而非 `&mut ThreadControlBlock`，因為：
/// 1. 允許巢狀 HandleScope（多個 scope 可以同時存在）
/// 2. 允許在 scope 期間存取 heap 等其他 TCB 功能
/// 3. 使用 UnsafeCell 實現內部可變性，由 level counter 保證正確性
/// 
/// # Example
/// 
/// ```rust
/// fn example(tcb: &ThreadControlBlock) {
///     let scope = HandleScope::new(tcb);
///     
///     let gc = Gc::new(42);
///     let handle = scope.handle(&gc);  // handle: Handle<'_, i32>
///     
///     // 可以巢狀建立 scope
///     {
///         let inner_scope = HandleScope::new(tcb);
///         let inner_handle = inner_scope.handle(&gc);
///     }  // inner_scope 結束，inner_handle 失效
///     
///     // handle 仍然有效
/// }  // scope 結束，handle 失效
/// ```
pub struct HandleScope<'env> {
    /// 關聯的 ThreadControlBlock (使用共享引用)
    tcb: &'env ThreadControlBlock,
    /// 進入 scope 前的 next 指標
    prev_next: *mut HandleSlot,
    /// 進入 scope 前的 limit 指標
    prev_limit: *mut HandleSlot,
    /// 進入 scope 前的 level
    prev_level: u32,
    /// 防止 Send/Sync
    _marker: PhantomData<*mut ()>,
}

impl<'env> HandleScope<'env> {
    /// 創建新的 HandleScope
    /// 
    /// # Arguments
    /// 
    /// * `tcb` - 執行緒控制區塊的共享引用
    /// 
    /// # Example
    /// 
    /// ```rust
    /// let scope = HandleScope::new(&tcb);
    /// ```
    #[inline]
    pub fn new(tcb: &'env ThreadControlBlock) -> Self {
        // SAFETY: 單執行緒存取，透過 level counter 保證正確的 scope 巢狀
        let scope_data_ptr = tcb.local_handles_ptr();
        
        let (prev_next, prev_limit, prev_level) = unsafe {
            let data = &mut *scope_data_ptr;
            let prev = (data.next, data.limit, data.level);
            data.level += 1;
            prev
        };
        
        Self {
            tcb,
            prev_next,
            prev_limit,
            prev_level,
            _marker: PhantomData,
        }
    }
    
    /// 在當前 scope 中創建 Handle
    /// 
    /// Handle 的生命週期綁定到 scope，無法逃逸。
    /// 
    /// # Panics
    /// 
    /// 在 debug build 中，如果在 SealedHandleScope 內呼叫會 panic。
    #[inline]
    pub fn handle<'scope, T: Trace>(&'scope self, gc: &Gc<T>) -> Handle<'scope, T> {
        let slot_ptr = self.allocate_slot();
        
        // 寫入 GcBox 指標到 slot
        let gc_box_ptr = Gc::internal_ptr(gc);
        unsafe {
            slot_ptr.write(HandleSlot::new(gc_box_ptr as *const GcBox<()>));
        }
        
        Handle {
            slot: slot_ptr,
            _marker: PhantomData,
        }
    }
    
    /// 分配一個 handle slot
    /// 
    /// # Safety
    /// 
    /// 使用原始指標操作避免建立 &mut 引用，符合 Rust 別名規則。
    #[inline]
    fn allocate_slot(&self) -> *mut HandleSlot {
        // SAFETY: 完全使用原始指標操作，不建立 &mut 引用
        let scope_data_ptr = self.tcb.local_handles_ptr();
        
        unsafe {
            #[cfg(debug_assertions)]
            {
                let data = &*scope_data_ptr;
                if data.level <= data.sealed_level {
                    panic!("cannot allocate handle in SealedHandleScope");
                }
            }
            
            let next = (*scope_data_ptr).next;
            let limit = (*scope_data_ptr).limit;
            
            if next == limit {
                // Block 已滿，分配新的
                self.tcb.add_handle_block()
            } else {
                (*scope_data_ptr).next = next.add(1);
                next
            }
        }
    }
    
    /// 取得當前 scope level (用於除錯)
    #[inline]
    pub fn level(&self) -> u32 {
        unsafe { (*self.tcb.local_handles_ptr()).level }
    }
}

impl Drop for HandleScope<'_> {
    fn drop(&mut self) {
        // SAFETY: 還原 scope 狀態，使用原始指標操作
        let scope_data_ptr = self.tcb.local_handles_ptr();
        
        unsafe {
            (*scope_data_ptr).next = self.prev_next;
            (*scope_data_ptr).limit = self.prev_limit;
            (*scope_data_ptr).level = self.prev_level;
        }
        
        // 回收未使用的 blocks
        self.tcb.remove_unused_blocks(self.prev_limit);
    }
}
```

### 3.2 EscapeableHandleScope

```rust
/// EscapeableHandleScope - 允許 Handle 逃逸到父 scope
/// 
/// 當需要將 handle 從內層 scope 傳遞到外層 scope 時使用。
/// 每個 EscapeableHandleScope 只能逃逸一個 handle。
/// 
/// # 設計決策：安全的逃逸機制
/// 
/// escape() 方法需要父 scope 引用作為參數，這確保：
/// 1. 返回的 Handle 生命週期正確綁定到父 scope
/// 2. 無法創建懸空 handle（編譯期保證）
/// 3. 避免了原設計中 'outer 參數可以隨意指定的問題
/// 
/// # Example
/// 
/// ```rust
/// fn create_value<'parent>(
///     parent: &'parent HandleScope<'_>,
///     tcb: &ThreadControlBlock,
/// ) -> Handle<'parent, i32> {
///     let escape_scope = EscapeableHandleScope::new(tcb);
///     
///     let gc = Gc::new(42);
///     let inner_handle = escape_scope.handle(&gc);
///     
///     // 將 handle 逃逸到父 scope - 必須提供父 scope 引用
///     escape_scope.escape(parent, inner_handle)
/// }
/// ```
pub struct EscapeableHandleScope<'env> {
    /// 內部的 HandleScope
    inner: HandleScope<'env>,
    /// 是否已經使用過 escape
    escaped: Cell<bool>,
    /// 逃逸 slot 的位置（在父 scope 中預先分配）
    escape_slot: *mut HandleSlot,
    /// 創建時的 parent level (用於驗證)
    #[cfg(debug_assertions)]
    parent_level: u32,
}

impl<'env> EscapeableHandleScope<'env> {
    /// 創建新的 EscapeableHandleScope
    /// 
    /// 會在父 scope 中預先分配一個 slot 用於逃逸。
    /// 
    /// # Note
    /// 
    /// 使用共享引用 &ThreadControlBlock，與 HandleScope 設計一致。
    #[inline]
    pub fn new(tcb: &'env ThreadControlBlock) -> Self {
        // 取得當前 parent level (用於驗證)
        #[cfg(debug_assertions)]
        let parent_level = unsafe { (*tcb.local_handles_ptr()).level };
        
        // 先在父 scope 分配逃逸用的 slot
        let escape_slot = Self::allocate_escape_slot(tcb);
        
        // 創建內部 scope
        let inner = HandleScope::new(tcb);
        
        Self {
            inner,
            escaped: Cell::new(false),
            escape_slot,
            #[cfg(debug_assertions)]
            parent_level,
        }
    }
    
    #[inline]
    fn allocate_escape_slot(tcb: &ThreadControlBlock) -> *mut HandleSlot {
        let scope_data_ptr = tcb.local_handles_ptr();
        
        unsafe {
            let next = (*scope_data_ptr).next;
            let limit = (*scope_data_ptr).limit;
            
            if next == limit {
                tcb.add_handle_block()
            } else {
                (*scope_data_ptr).next = next.add(1);
                next
            }
        }
    }
    
    /// 在當前 scope 中創建 Handle
    #[inline]
    pub fn handle<'scope, T: Trace>(&'scope self, gc: &Gc<T>) -> Handle<'scope, T> {
        self.inner.handle(gc)
    }
    
    /// 將 handle 逃逸到父 scope
    /// 
    /// # 安全的逃逸設計
    /// 
    /// 此方法需要父 scope 的引用，這樣返回的 Handle 生命週期會正確
    /// 綁定到 parent 的生命週期，避免創建懸空指標。
    /// 
    /// # Arguments
    /// 
    /// * `parent` - 父 scope 的引用，用於綁定返回 Handle 的生命週期
    /// * `handle` - 要逃逸的 Handle
    /// 
    /// # Panics
    /// 
    /// - 如果已經呼叫過一次 escape，會 panic
    /// - 每個 EscapeableHandleScope 只能逃逸一個 handle
    /// - Debug 模式下，如果 parent 不是實際的父 scope，會 panic
    #[inline]
    pub fn escape<'parent, T: Trace>(
        &self,
        parent: &'parent HandleScope<'_>,
        handle: Handle<'_, T>,
    ) -> Handle<'parent, T> {
        if self.escaped.get() {
            panic!("EscapeableHandleScope::escape() can only be called once");
        }
        
        #[cfg(debug_assertions)]
        {
            // 驗證 parent 確實是我們的父 scope
            let current_level = self.inner.level();
            if parent.level() + 1 != current_level {
                panic!(
                    "escape() called with incorrect parent scope: expected level {}, got {}",
                    current_level - 1,
                    parent.level()
                );
            }
        }
        
        self.escaped.set(true);
        
        // 將 handle 的內容複製到預先分配的逃逸 slot
        unsafe {
            let slot_content = handle.slot.read();
            self.escape_slot.write(slot_content);
        }
        
        Handle {
            slot: self.escape_slot,
            _marker: PhantomData,
        }
    }
    
    /// 替代方法：使用閉包模式逃逸
    /// 
    /// 這個 API 更簡潔，適合函數內部使用。
    /// 閉包的返回值會被逃逸到外層。
    /// 
    /// # Example
    /// 
    /// ```rust
    /// let result = escape_scope.close_and_escape(|scope| {
    ///     let gc = Gc::new(42);
    ///     scope.handle(&gc)
    /// });
    /// ```
    #[inline]
    pub fn close_and_escape<'parent, T: Trace, F>(
        self,
        parent: &'parent HandleScope<'_>,
        f: F,
    ) -> Handle<'parent, T>
    where
        F: FnOnce(&Self) -> Handle<'_, T>,
    {
        let inner_handle = f(&self);
        
        // scope 結束時，將 handle 複製到預先分配的 slot
        unsafe {
            let slot_content = inner_handle.slot.read();
            self.escape_slot.write(slot_content);
        }
        
        // inner scope 會在 self drop 時自動結束
        Handle {
            slot: self.escape_slot,
            _marker: PhantomData,
        }
    }
}

impl<'env> std::ops::Deref for EscapeableHandleScope<'env> {
    type Target = HandleScope<'env>;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}
```

### 3.3 SealedHandleScope

```rust
/// SealedHandleScope - 封印 scope，禁止新 handle 創建
/// 
/// 用於確保某段程式碼不會創建新的 handles。
/// 主要用於 GC 期間或效能敏感的區域。
/// 
/// # 設計說明
/// 
/// 使用 `sealed_level` 而非 `limit` 操作，這是 V8 的設計模式：
/// - 設置 `sealed_level = level`，表示在這個 level 或以下禁止分配
/// - `allocate_slot()` 會檢查 `level <= sealed_level` 並 panic
/// - 這比操作 limit 更可靠，因為 limit 可能被 add_block 覆蓋
/// 
/// # Example
/// 
/// ```rust
/// {
///     let _seal = SealedHandleScope::new(&tcb);
///     // 這裡嘗試創建 handle 會 panic (debug mode)
///     // 可以創建新的 HandleScope 來解除封印
///     {
///         let scope = HandleScope::new(&tcb);
///         let handle = scope.handle(&gc);  // OK - new scope level
///     }
/// }
/// ```
#[cfg(debug_assertions)]
pub struct SealedHandleScope<'env> {
    tcb: &'env ThreadControlBlock,
    prev_sealed_level: u32,
}

#[cfg(debug_assertions)]
impl<'env> SealedHandleScope<'env> {
    pub fn new(tcb: &'env ThreadControlBlock) -> Self {
        let scope_data_ptr = tcb.local_handles_ptr();
        
        let prev_sealed_level = unsafe {
            let data = &mut *scope_data_ptr;
            let prev = data.sealed_level;
            // 設置 sealed_level 為當前 level，禁止在此 level 分配
            data.sealed_level = data.level;
            prev
        };
        
        Self { tcb, prev_sealed_level }
    }
}

#[cfg(debug_assertions)]
impl Drop for SealedHandleScope<'_> {
    fn drop(&mut self) {
        let scope_data_ptr = self.tcb.local_handles_ptr();
        unsafe {
            (*scope_data_ptr).sealed_level = self.prev_sealed_level;
        }
    }
}

#[cfg(not(debug_assertions))]
pub struct SealedHandleScope<'env>(PhantomData<&'env ()>);

#[cfg(not(debug_assertions))]
impl<'env> SealedHandleScope<'env> {
    #[inline]
    pub fn new(_tcb: &'env ThreadControlBlock) -> Self {
        Self(PhantomData)
    }
}
```

---

## 4. Handle 類型

### 4.1 Handle<'scope, T>

```rust
/// Handle - 帶生命週期的 GC 指標引用
/// 
/// Handle 是對 Gc<T> 的安全引用，其生命週期綁定到創建它的 HandleScope。
/// 當 scope 結束時，所有綁定到該 scope 的 handles 都會自動失效。
/// 
/// # 生命週期安全
/// 
/// ```rust
/// let handle;
/// {
///     let scope = HandleScope::new(&mut tcb);
///     let gc = Gc::new(42);
///     handle = scope.handle(&gc);
/// }  // scope 結束
/// // *handle;  // 編譯錯誤！handle 的生命週期已結束
/// ```
/// 
/// # 與 Gc<T> 的關係
/// 
/// - `Gc<T>`: 擁有 GcBox 的一個引用計數
/// - `Handle<'s, T>`: 透過 HandleScope 追蹤的臨時引用，不影響引用計數
pub struct Handle<'scope, T: Trace> {
    /// 指向 HandleSlot 的指標
    slot: *const HandleSlot,
    /// 生命週期和類型標記
    _marker: PhantomData<(&'scope (), *const T)>,
}

impl<'scope, T: Trace> Handle<'scope, T> {
    /// 取得 Handle 指向的值的引用
    /// 
    /// # Example
    /// 
    /// ```rust
    /// let handle = scope.handle(&gc);
    /// println!("{}", handle.get());
    /// ```
    #[inline]
    pub fn get(&self) -> &T {
        unsafe {
            let slot = &*self.slot;
            let gc_box = slot.cast::<T>().as_ref();
            &gc_box.value
        }
    }
    
    /// 取得內部的原始 GcBox 指標
    /// 
    /// # Safety
    /// 
    /// 回傳的指標只在 HandleScope 有效期間有效。
    #[inline]
    pub unsafe fn as_ptr(&self) -> *const GcBox<T> {
        let slot = &*self.slot;
        slot.cast::<T>().as_ptr()
    }
    
    /// 從 Handle 創建 Gc
    /// 
    /// 這會增加 GcBox 的引用計數。
    #[inline]
    pub fn to_gc(&self) -> Gc<T> {
        unsafe {
            let gc_box_ptr = self.as_ptr();
            (*gc_box_ptr).inc_ref();
            Gc::from_raw(gc_box_ptr as *const u8)
        }
    }
}

impl<T: Trace> std::ops::Deref for Handle<'_, T> {
    type Target = T;
    
    #[inline]
    fn deref(&self) -> &Self::Target {
        self.get()
    }
}

impl<T: Trace + std::fmt::Debug> std::fmt::Debug for Handle<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Handle")
            .field(self.get())
            .finish()
    }
}

impl<T: Trace + std::fmt::Display> std::fmt::Display for Handle<'_, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.get().fmt(f)
    }
}

// Handle 不是 Send/Sync，因為它綁定到特定執行緒的 HandleScope
impl<T: Trace> !Send for Handle<'_, T> {}
impl<T: Trace> !Sync for Handle<'_, T> {}

impl<T: Trace> Clone for Handle<'_, T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: Trace> Copy for Handle<'_, T> {}
```

### 4.2 MaybeHandle<'scope, T>

```rust
/// MaybeHandle - 可能為空的 Handle
/// 
/// 類似 Option<Handle<'s, T>>，但具有更好的記憶體佈局。
pub struct MaybeHandle<'scope, T: Trace> {
    slot: *const HandleSlot,
    _marker: PhantomData<(&'scope (), *const T)>,
}

impl<'scope, T: Trace> MaybeHandle<'scope, T> {
    /// 創建空的 MaybeHandle
    #[inline]
    pub const fn empty() -> Self {
        Self {
            slot: std::ptr::null(),
            _marker: PhantomData,
        }
    }
    
    /// 從 Handle 創建 MaybeHandle
    #[inline]
    pub fn from_handle(handle: Handle<'scope, T>) -> Self {
        Self {
            slot: handle.slot,
            _marker: PhantomData,
        }
    }
    
    /// 檢查是否為空
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.slot.is_null()
    }
    
    /// 轉換為 Option<Handle>
    #[inline]
    pub fn to_handle(self) -> Option<Handle<'scope, T>> {
        if self.slot.is_null() {
            None
        } else {
            Some(Handle {
                slot: self.slot,
                _marker: PhantomData,
            })
        }
    }
}
```

---

## 5. Async 整合

### 5.1 AsyncHandleScope

```rust
use std::sync::atomic::{AtomicU64, AtomicUsize, AtomicBool, Ordering};

/// 全域 async scope ID 計數器
static NEXT_ASYNC_SCOPE_ID: AtomicU64 = AtomicU64::new(0);

/// AsyncHandleScope - 支援 async/await 的 HandleScope
/// 
/// 解決 async task 中 GC roots 追蹤的問題。當 task 被暫停時，
/// AsyncHandleScope 會確保所有 handles 被正確追蹤。
/// 
/// # 設計決策
/// 
/// 1. **ID-based 註冊**：使用唯一 ID 而非指標進行註冊/反註冊，
///    避免自引用結構的問題。
/// 
/// 2. **安全存取模式**：提供 `with_guard()` 方法返回 Guard 類型，
///    生命週期綁定確保安全存取。
/// 
/// 3. **精確根集合**：handles 已經指向 GcBox 頭部，不需要 find_gc_box_from_ptr。
/// 
/// # Example
/// 
/// ```rust
/// async fn async_example(tcb: Arc<ThreadControlBlock>) {
///     let scope = AsyncHandleScope::new(&tcb);
///     
///     let gc = Gc::new(42);
///     let handle = scope.handle(&gc);
///     
///     some_async_operation().await;  // task 可能被暫停
///     
///     // 使用 guard 安全存取
///     scope.with_guard(|guard| {
///         println!("{}", guard.get(&handle));
///     });
/// }
/// ```
pub struct AsyncHandleScope {
    /// 唯一 ID (用於 TCB 註冊)
    id: u64,
    /// 關聯的 TCB (Arc 以支援跨 await)
    tcb: Arc<ThreadControlBlock>,
    /// 專屬的 handle block (不與同步 scope 共用)
    block: Box<HandleBlock>,
    /// 已使用的 slot 數量
    used: AtomicUsize,
    /// 是否已 drop (用於 debug 驗證)
    dropped: AtomicBool,
}

impl AsyncHandleScope {
    /// 創建新的 AsyncHandleScope
    pub fn new(tcb: &Arc<ThreadControlBlock>) -> Self {
        let id = NEXT_ASYNC_SCOPE_ID.fetch_add(1, Ordering::Relaxed);
        
        let scope = Self {
            id,
            tcb: Arc::clone(tcb),
            block: HandleBlock::new(),
            used: AtomicUsize::new(0),
            dropped: AtomicBool::new(false),
        };
        
        // 使用 ID 和 block 指標註冊
        tcb.register_async_scope(id, scope.block.as_ref() as *const _);
        
        scope
    }
    
    /// 取得唯一 ID
    pub fn id(&self) -> u64 {
        self.id
    }
    
    /// 在 async scope 中創建 Handle
    /// 
    /// 返回 AsyncHandle，可與 `with_guard()` 配合使用。
    pub fn handle<T: Trace>(&self, gc: &Gc<T>) -> AsyncHandle<T> {
        let index = self.used.fetch_add(1, Ordering::Relaxed);
        
        if index >= HANDLE_BLOCK_SIZE {
            panic!("AsyncHandleScope: too many handles (max {})", HANDLE_BLOCK_SIZE);
        }
        
        let slot_ptr = unsafe { 
            self.block.slots.as_ptr().add(index) as *mut HandleSlot 
        };
        
        unsafe {
            slot_ptr.write(HandleSlot::new(Gc::internal_ptr(gc) as *const GcBox<()>));
        }
        
        AsyncHandle {
            slot: slot_ptr,
            scope_id: self.id,
            _marker: PhantomData,
        }
    }
    
    /// 安全存取 handle 的模式
    /// 
    /// 使用 closure 確保 handle 存取在 scope 存活期間進行。
    #[inline]
    pub fn with_guard<F, R>(&self, f: F) -> R
    where
        F: FnOnce(AsyncHandleGuard<'_>) -> R,
    {
        let guard = AsyncHandleGuard {
            scope: self,
            _marker: PhantomData,
        };
        f(guard)
    }
    
    /// 遍歷所有 handles (GC 時呼叫)
    /// 
    /// 注意：handles 已經是精確的 GcBox 指標，不需要使用 find_gc_box_from_ptr。
    pub fn iterate(&self, visitor: &mut GcVisitor) {
        let used = self.used.load(Ordering::Acquire);

        for i in 0..used {
            let slot = unsafe {
                &*self.block.slots.as_ptr().add(i).cast::<HandleSlot>()
            };
            
            let gc_box_ptr = slot.gc_box_ptr;
            if !gc_box_ptr.is_null() {
                unsafe {
                    visitor.mark_root(NonNull::new_unchecked(gc_box_ptr as *mut GcBox<()>));
                }
            }
        }
    }
}

impl Drop for AsyncHandleScope {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
        // 使用 ID 反註冊
        self.tcb.unregister_async_scope(self.id);
    }
}

// AsyncHandleScope 是 Send，因為它使用 Arc<TCB>
unsafe impl Send for AsyncHandleScope {}
```

### 5.2 AsyncHandleGuard

```rust
/// AsyncHandleGuard - 安全存取 AsyncHandle 的 guard
/// 
/// 生命週期綁定到 AsyncHandleScope，確保 handle 在存取期間有效。
pub struct AsyncHandleGuard<'scope> {
    scope: &'scope AsyncHandleScope,
    _marker: PhantomData<&'scope ()>,
}

impl<'scope> AsyncHandleGuard<'scope> {
    /// 安全地取得 handle 的值引用
    /// 
    /// 生命週期 'scope 確保 handle 在存取期間有效。
    #[inline]
    pub fn get<T: Trace>(&self, handle: &AsyncHandle<T>) -> &T {
        // Debug 模式下驗證 handle 屬於此 scope
        #[cfg(debug_assertions)]
        {
            if handle.scope_id != self.scope.id {
                panic!(
                    "AsyncHandle belongs to scope {} but accessed from scope {}",
                    handle.scope_id, self.scope.id
                );
            }
        }
        
        unsafe {
            let slot = &*handle.slot;
            let gc_box = slot.cast::<T>().as_ref();
            &gc_box.value
        }
    }
}
```

### 5.3 AsyncHandle<T>

```rust
/// AsyncHandle - async 環境中的 Handle
/// 
/// 與 Handle<'scope, T> 不同，AsyncHandle 沒有生命週期參數，
/// 因為它的有效性由 AsyncHandleScope 管理。
/// 
/// # 安全存取模式
/// 
/// 推薦使用 `scope.with_guard()` 進行安全存取：
/// 
/// ```rust
/// let handle = scope.handle(&gc);
/// 
/// scope.with_guard(|guard| {
///     let value = guard.get(&handle);
///     // 使用 value
/// });
/// ```
/// 
/// # 直接存取 (unsafe)
/// 
/// 也可以直接使用 `get()`，但需要確保 scope 仍然存活：
/// 
/// ```rust
/// let value = unsafe { handle.get() };  // 呼叫者負責確保安全性
/// ```
pub struct AsyncHandle<T: Trace> {
    slot: *const HandleSlot,
    /// 所屬 scope 的 ID (用於 debug 驗證)
    scope_id: u64,
    _marker: PhantomData<*const T>,
}

impl<T: Trace> AsyncHandle<T> {
    /// 取得值的引用 (unsafe)
    /// 
    /// # Safety
    /// 
    /// 呼叫者必須確保對應的 AsyncHandleScope 仍然存活。
    /// 推薦使用 `scope.with_guard()` 進行安全存取。
    #[inline]
    pub unsafe fn get(&self) -> &T {
        let slot = &*self.slot;
        let gc_box = slot.cast::<T>().as_ref();
        &gc_box.value
    }
    
    /// 轉換為 Gc<T>
    /// 
    /// 這是安全的，因為它會增加引用計數。
    #[inline]
    pub fn to_gc(&self) -> Gc<T> {
        unsafe {
            let slot = &*self.slot;
            let gc_box_ptr = slot.cast::<T>().as_ptr();
            (*gc_box_ptr).inc_ref();
            Gc::from_raw(gc_box_ptr as *const u8)
        }
    }
    
    /// 取得所屬 scope 的 ID
    pub fn scope_id(&self) -> u64 {
        self.scope_id
    }
}

impl<T: Trace> Copy for AsyncHandle<T> {}
impl<T: Trace> Clone for AsyncHandle<T> {
    fn clone(&self) -> Self {
        *self
    }
}

unsafe impl<T: Trace> Send for AsyncHandle<T> {}
```

### 5.3 spawn_with_gc! Macro

```rust
/// spawn_with_gc! - 安全地在 tokio::spawn 中使用 Gc
/// 
/// 這是推薦的 async GC 使用方式，確保 GC roots 被正確追蹤。
/// 
/// # Example
/// 
/// ```rust
/// let gc = Gc::new(MyData { value: 42 });
/// 
/// spawn_with_gc!(gc => |handle| async move {
///     println!("{}", handle.get().value);
///     some_async_op().await;
///     println!("{}", handle.get().value);
/// });
/// ```
#[macro_export]
macro_rules! spawn_with_gc {
    /// Spawn an async task with GC root tracking for a single Gc
    ///
    /// # Example
    ///
    /// ```rust
    /// let gc = Gc::new(MyData { value: 42 });
    ///
    /// spawn_with_gc!(gc => |handle| async move {
    ///     println!("{}", handle.get().value);
    ///     some_async_op().await;
    ///     println!("{}", handle.get().value);
    /// });
    /// ```
    ($gc:expr => |$handle:ident| $body:expr) => {{
        let __gc = $gc;
        let __tcb = $crate::heap::current_thread_control_block()
            .expect("spawn_with_gc! must be called within a GC thread");

        tokio::spawn(async move {
            let __scope = $crate::AsyncHandleScope::new(&__tcb);
            let $handle = __scope.handle(&__gc);

            let __result = { $body.await };
            drop(__scope);
            __result
        })
    }};

    /// Spawn an async task with GC root tracking for multiple Gc
    ($($gc:ident),+ => |$($handle:ident),+| $body:expr) => {{
        $(let __gc = $gc;)+
        let __tcb = $crate::heap::current_thread_control_block()
            .expect("spawn_with_gc! must be called within a GC thread");

        tokio::spawn(async move {
            let __scope = $crate::AsyncHandleScope::new(&__tcb);
            $(let $handle = __scope.handle(&__gc);)+

            let __result = { $body.await };
            drop(__scope);
            __result
        })
    }};
}
```

---

## 6. Interior Pointer 支援

### 6.0 Bug 修復記錄 (2026-02-01)

在實作規格時，我們發現現有程式碼 `find_gc_box_from_ptr` 有以下問題：

#### 問題描述

**Bug**: `find_gc_box_from_ptr` 要求指標必須對齊到 `usize`（8 bytes on x86_64）

```rust
// heap.rs:2180-2183 (修復前)
if addr % std::mem::align_of::<usize>() != 0 {
    return None;  // ← 這裡拒絕了非 usize 對齊的 interior pointer
}
```

**影響**: 當 interior pointer 指向 `u32` 欄位時會失敗

```
// 測試輸出
ptr_y % 8: 4 (aligned: false)  // u32 欄位，4-byte 對齊
box_from_y: None               // ❌ 被錯誤拒絕
```

#### 修復方案

移除 `usize` 對齊限制，因為 interior pointer 可以指向任何欄位：

```rust
// heap.rs:2179-2186 (修復後)
// 2. Interior pointer support: allow pointers to any field, not just usize-aligned.
//    A u32 field may be at offset 4, which is valid for u32 but not for usize (8-byte).
//    For conservative GC, we need to accept any potentially valid pointer alignment.
//    Minimum alignment is 1 byte (no alignment requirement for interior pointers).
unsafe {
    // Note: We removed the usize alignment check here to support interior pointers
    // to fields smaller than usize (e.g., u32, u16, u8). The page header and offset
    // calculations will validate whether this is a valid object pointer.
```

**測試結果**: 所有 interior pointer 測試通過 ✅

---

### 6.1 實作細節

我們直接增強了核心 API `find_gc_box_from_ptr` 以支援 interior pointer，而不是新增獨立 API。

```rust
/// 從任意指標（包括 interior pointer）找到對應的 GcBox
/// 
/// 這是 v2 的關鍵修正，完整支援 interior pointer。
/// 
/// # Algorithm
/// 
/// 1. 從指標計算所在的 page header
/// 2. 從 page header 取得 object size class
/// 3. 計算指標所在的 object index (向下取整)
/// 4. 驗證該 index 是否指向有效的已分配 object
/// 5. 回傳 object 的起始位置
/// 
/// # Safety
///
/// 此函數可能回傳無效指標，呼叫者必須驗證回傳值。
pub unsafe fn find_gc_box_from_ptr(
    heap: &LocalHeap,
    ptr: *const u8,
) -> Option<NonNull<GcBox<()>>> {
    if ptr.is_null() {
        return None;
    }
    
    // 取得 page header
    let header = ptr_to_page_header(ptr);
    
    // 驗證 magic number
    if (*header.as_ptr()).magic != MAGIC_GC_PAGE {
        return None;
    }
    
    let page_base = header.as_ptr() as usize + PAGE_HEADER_SIZE;
    let ptr_addr = ptr as usize;
    
    // 檢查指標是否在頁面範圍內
    if ptr_addr < page_base {
        return None;
    }
    
    let offset = ptr_addr - page_base;
    let block_size = (*header.as_ptr()).block_size as usize;
    
    if block_size == 0 {
        return None;
    }
    
    // === 關鍵修正：Interior Pointer 支援 ===
    // 向下取整計算 object index，而不是要求精確對齊
    let object_index = offset / block_size;
    
    // 驗證 index 在有效範圍內
    let max_objects = (*header.as_ptr()).capacity as usize;
    if object_index >= max_objects {
        return None;
    }
    
    // 驗證該 slot 是否已分配
    if !(*header.as_ptr()).is_allocated(object_index) {
        return None;
    }
    
    // 計算 object 的起始位置
    let object_ptr = page_base + object_index * block_size;
    
    NonNull::new(object_ptr as *mut GcBox<()>)
}
```

### 6.2 測試案例

```rust
#[cfg(test)]
mod interior_pointer_tests {
    use super::*;
    
    #[derive(Trace)]
    struct Node {
        a: u64,
        b: u64,
        c: u64,
    }
    
    #[test]
    fn test_interior_pointer_basic() {
        let gc = Gc::new(Node { a: 1, b: 2, c: 3 });
        
        // 取得各欄位的指標
        let ptr_a = &gc.a as *const u64 as *const u8;
        let ptr_b = &gc.b as *const u64 as *const u8;
        let ptr_c = &gc.c as *const u64 as *const u8;
        
        // 所有 interior pointer 都應該能找到同一個 GcBox
        unsafe {
            let box_from_a = find_gc_box_from_ptr(ptr_a);
            let box_from_b = find_gc_box_from_ptr(ptr_b);
            let box_from_c = find_gc_box_from_ptr(ptr_c);
            
            assert!(box_from_a.is_some());
            assert!(box_from_b.is_some());
            assert!(box_from_c.is_some());
            
            // 應該都指向同一個 GcBox
            assert_eq!(box_from_a, box_from_b);
            assert_eq!(box_from_b, box_from_c);
        }
    }
    
    #[test]
    fn test_interior_pointer_gc_survival() {
        let gc = Gc::new(Node { a: 1, b: 2, c: 3 });
        let ref_b: *const u64 = &gc.b;
        
        // 模擬只有 interior pointer 在 stack 上的情況
        drop(gc);
        
        // 觸發 GC
        crate::collect();
        
        // 如果 interior pointer 支援正確，物件應該存活
        // (實際測試需要更複雜的設置)
    }
}
```

---

## 7. ThreadControlBlock 擴展

### 7.1 更新後的 ThreadControlBlock

```rust
/// AsyncScopeEntry - 追蹤已註冊的 async scope
struct AsyncScopeEntry {
    id: u64,
    block: *const HandleBlock,
}

/// 擴展 ThreadControlBlock 以支援 HandleScope
pub struct ThreadControlBlock {
    // 原有欄位
    pub state: AtomicUsize,
    pub gc_requested: AtomicBool,
    pub park_cond: Condvar,
    pub park_mutex: Mutex<()>,
    pub heap: UnsafeCell<LocalHeap>,
    pub stack_roots: Mutex<Vec<*const u8>>,
    
    // === v2 新增 ===
    /// Handle 管理器 (使用 UnsafeCell 實現內部可變性)
    local_handles: UnsafeCell<LocalHandles>,
    /// Async scopes 列表 (使用 ID 而非指標)
    async_scopes: Mutex<Vec<AsyncScopeEntry>>,
}

impl ThreadControlBlock {
    pub fn new() -> Self {
        Self {
            state: AtomicUsize::new(THREAD_STATE_RUNNING),
            gc_requested: AtomicBool::new(false),
            park_cond: Condvar::new(),
            park_mutex: Mutex::new(()),
            heap: UnsafeCell::new(LocalHeap::new()),
            stack_roots: Mutex::new(Vec::new()),
            // v2 新增
            local_handles: UnsafeCell::new(LocalHandles::new()),
            async_scopes: Mutex::new(Vec::new()),
        }
    }
    
    // === HandleScope 支援方法 ===
    
    /// 取得 HandleScopeData 的原始指標
    /// 
    /// 這是 HandleScope 內部使用的方法，返回原始指標以避免
    /// 創建 &mut 引用的別名問題。
    /// 
    /// # Safety
    /// 
    /// 呼叫者必須確保單執行緒存取。HandleScope 的 level counter
    /// 機制確保了正確的巢狀順序。
    #[inline]
    pub fn local_handles_ptr(&self) -> *mut HandleScopeData {
        unsafe { 
            let handles = &mut *self.local_handles.get();
            handles.scope_data_ptr()
        }
    }
    
    /// 取得 LocalHandles 的可變引用 (legacy API)
    /// 
    /// 注意：新程式碼應使用 `local_handles_ptr()` 以避免別名問題。
    #[inline]
    pub fn local_handles_mut(&mut self) -> &mut LocalHandles {
        self.local_handles.get_mut()
    }
    
    /// 分配新的 HandleBlock 並返回第一個 slot 的指標
    /// 
    /// 由 HandleScope 在 block 滿時呼叫。
    #[inline]
    pub fn add_handle_block(&self) -> *mut HandleSlot {
        unsafe {
            let handles = &mut *self.local_handles.get();
            handles.add_block()
        }
    }
    
    /// 移除超過指定 limit 的未使用 blocks
    /// 
    /// 由 HandleScope::drop 呼叫以回收記憶體。
    #[inline]
    pub fn remove_unused_blocks(&self, limit: *mut HandleSlot) {
        unsafe {
            let handles = &mut *self.local_handles.get();
            handles.remove_unused_blocks(limit);
        }
    }
    
    // === Async Scope 管理 ===
    
    /// 註冊 AsyncHandleScope (使用 ID)
    pub fn register_async_scope(&self, id: u64, block: *const HandleBlock) {
        let mut scopes = self.async_scopes.lock().unwrap();
        scopes.push(AsyncScopeEntry { id, block });
    }
    
    /// 取消註冊 AsyncHandleScope (使用 ID)
    pub fn unregister_async_scope(&self, id: u64) {
        let mut scopes = self.async_scopes.lock().unwrap();
        scopes.retain(|entry| entry.id != id);
    }
    
    // === GC 根集合遍歷 ===
    
    /// 遍歷所有 handles (GC 時呼叫)
    /// 
    /// 這是精確根集合收集，不需要 conservative scanning。
    pub fn iterate_all_handles(&self, visitor: &mut GcVisitor) {
        // 遍歷同步 handles
        unsafe {
            (*self.local_handles.get()).iterate(visitor);
        }

        // 遍歷 async scopes 的 handles
        let scopes = self.async_scopes.lock().unwrap();
        for entry in scopes.iter() {
            unsafe {
                // 直接遍歷已知的 block，不需要解引用 AsyncHandleScope
                iterate_handle_block(entry.block, visitor);
            }
        }
    }
}

/// 遍歷 HandleBlock 中的所有 handles
/// 
/// 用於 async scope 的 GC 根收集。
unsafe fn iterate_handle_block(block: *const HandleBlock, visitor: &mut GcVisitor) {
    if block.is_null() {
        return;
    }
    
    let block_ref = &*block;
    // 假設 block 已滿或使用 atomic counter 追蹤使用量
    // 這裡簡化處理，實際實作需配合 AsyncHandleScope::used
    for slot in block_ref.slots.iter() {
        let slot = slot.assume_init_ref();
        let gc_box_ptr = slot.gc_box_ptr;
        if !gc_box_ptr.is_null() {
            visitor.mark_root(NonNull::new_unchecked(gc_box_ptr as *mut GcBox<()>));
        }
    }
}
```

### 7.2 LocalHandles 完整定義

```rust
/// LocalHandles - 管理每個執行緒的 handle 儲存
pub struct LocalHandles {
    /// HandleBlock 連結串列的頭部
    blocks: Option<NonNull<HandleBlock>>,
    /// 當前 scope 的分配狀態
    scope_data: HandleScopeData,
}

impl LocalHandles {
    pub const fn new() -> Self {
        Self {
            blocks: None,
            scope_data: HandleScopeData::new(),
        }
    }
    
    /// 取得 scope_data 的原始指標 (避免別名問題)
    #[inline]
    pub fn scope_data_ptr(&mut self) -> *mut HandleScopeData {
        &mut self.scope_data as *mut HandleScopeData
    }
    
    /// 取得 scope_data 的可變引用 (legacy API)
    #[inline]
    pub fn scope_data_mut(&mut self) -> &mut HandleScopeData {
        &mut self.scope_data
    }
    
    /// 分配新的 block
    pub fn add_block(&mut self) -> *mut HandleSlot {
        #[cfg(debug_assertions)]
        {
            // 檢查是否在 sealed scope 中
            if self.scope_data.level <= self.scope_data.sealed_level {
                panic!("cannot allocate handle in SealedHandleScope");
            }
        }
        
        // 分配新 block
        let mut new_block = HandleBlock::new();
        new_block.next = self.blocks;
        
        let block_ptr = NonNull::new(Box::into_raw(new_block)).unwrap();
        self.blocks = Some(block_ptr);
        
        // 更新 scope_data
        let first_slot = unsafe { block_ptr.as_ref().slots.as_ptr() as *mut HandleSlot };
        self.scope_data.next = unsafe { first_slot.add(1) };
        self.scope_data.limit = unsafe { first_slot.add(HANDLE_BLOCK_SIZE) };
        
        first_slot
    }
    
    /// 移除超過 limit 的未使用 blocks
    /// 
    /// 當 HandleScope drop 時呼叫。
    pub fn remove_unused_blocks(&mut self, prev_limit: *mut HandleSlot) {
        if prev_limit.is_null() {
            // 所有 blocks 都應該被釋放
            self.free_all_blocks();
            return;
        }
        
        // 找到 prev_limit 所在的 block 並移除之後的所有 blocks
        // 這裡簡化實作 - 實際需要更複雜的連結串列操作
        // V8 使用 DeleteExtensions 來處理類似情況
    }
    
    /// 釋放所有 blocks
    fn free_all_blocks(&mut self) {
        let mut current = self.blocks;
        while let Some(block_ptr) = current {
            let block = unsafe { Box::from_raw(block_ptr.as_ptr()) };
            current = block.next;
        }
        self.blocks = None;
        self.scope_data = HandleScopeData::new();
    }
    
    /// 遍歷所有 handles (GC 時呼叫)
    /// 
    /// 精確遍歷，不需要 find_gc_box_from_ptr。
    pub fn iterate(&self, visitor: &mut GcVisitor) {
        let mut current_block = self.blocks;
        
        while let Some(block_ptr) = current_block {
            let block = unsafe { block_ptr.as_ref() };
            
            // 計算這個 block 中的有效 slot 範圍
            let start = block.slots.as_ptr() as *const HandleSlot;
            let block_end = if block.next.is_none() {
                // 這是最新的 block，使用 scope_data.next
                self.scope_data.next as *const HandleSlot
            } else {
                // 這是舊的 block，全部使用
                unsafe { start.add(HANDLE_BLOCK_SIZE) }
            };
            
            // 遍歷 slots
            let mut current = start;
            while current < block_end {
                let slot = unsafe { &*current };
                let gc_box_ptr = slot.gc_box_ptr;
                
                if !gc_box_ptr.is_null() {
                    unsafe {
                        visitor.mark_root(NonNull::new_unchecked(gc_box_ptr as *mut GcBox<()>));
                    }
                }
                
                current = unsafe { current.add(1) };
            }
            
            current_block = block.next;
        }
    }
}
```

### 7.3 current_thread_control_block 函數

```rust
// crates/rudo-gc/src/heap.rs

thread_local! {
    /// 當前執行緒的 TCB
    static CURRENT_TCB: RefCell<Option<Arc<ThreadControlBlock>>> = RefCell::new(None);
}

/// 取得當前執行緒的 ThreadControlBlock
/// 
/// 這是 `spawn_with_gc!` macro 使用的函數。
/// 
/// # Returns
/// 
/// - `Some(Arc<ThreadControlBlock>)` 如果在 GC 執行緒中
/// - `None` 如果不在 GC 執行緒中
pub fn current_thread_control_block() -> Option<Arc<ThreadControlBlock>> {
    CURRENT_TCB.with(|tcb| tcb.borrow().clone())
}

/// 設置當前執行緒的 ThreadControlBlock
/// 
/// 在執行緒註冊到 GC 系統時呼叫。
pub fn set_current_thread_control_block(tcb: Arc<ThreadControlBlock>) {
    CURRENT_TCB.with(|cell| {
        *cell.borrow_mut() = Some(tcb);
    });
}

/// 清除當前執行緒的 ThreadControlBlock
/// 
/// 在執行緒從 GC 系統反註冊時呼叫。
pub fn clear_current_thread_control_block() {
    CURRENT_TCB.with(|cell| {
        *cell.borrow_mut() = None;
    });
}
```

---

## 8. GC 整合

### 8.1 Root 收集修改

```rust
/// v2: 使用 HandleScope 的精確 root 收集
///
/// 這取代了原本的 conservative stack scanning。
fn collect_roots(heap: &LocalHeap, visitor: &mut GcVisitor) {
    let registry = thread_registry().lock().unwrap();

    for tcb in registry.threads.iter() {
        // 使用精確的 handle 遍歷
        tcb.iterate_all_handles(heap, visitor);

        // v2: 可選的 conservative fallback
        #[cfg(feature = "conservative-fallback")]
        {
            // 僅在 handlescope 模式下作為備份
            unsafe {
                crate::stack::spill_registers_and_scan(|potential_ptr, _addr, _is_reg| {
                    // 避免重複掃描已由 handle 追蹤的區域
                    if !is_in_handle_block(potential_ptr) {
                        if let Some(gc_box_ptr) = crate::heap::find_gc_box_from_ptr(
                            heap,
                            potential_ptr as *const u8
                        ) {
                            visitor.mark_root(gc_box_ptr);
                        }
                    }
                });
            }
        }
    }
}
```

### 8.2 Feature Flags

```toml
# Cargo.toml

[features]
default = ["handle-scope"]

# 啟用 HandleScope (v2 預設)
handle-scope = []

# 保留 conservative scanning 作為 fallback
conservative-fallback = []

# Async 支援
async = ["tokio"]

# 完整安全模式 (推薦)
safe = ["handle-scope"]

# 向後相容模式 (使用 v1 行為)
legacy = ["conservative-fallback"]
```

---

## 9. 遷移指南

### 9.1 從 v1 遷移到 v2

#### Step 1: 引入 HandleScope

```rust
// v1: 隱式 root tracking
fn example() {
    let gc = Gc::new(42);
    // gc 透過 conservative scanning 被追蹤
}

// v2: 顯式 HandleScope
fn example(tcb: &mut ThreadControlBlock) {
    let scope = HandleScope::new(tcb);
    
    let gc = Gc::new(42);
    let handle = scope.handle(&gc);
    // handle 透過 HandleScope 被精確追蹤
}
```

#### Step 2: 更新 async 程式碼

```rust
// v1: 需要手動 root_guard
let gc = Gc::new(42);
tokio::spawn(async move {
    let _guard = gc.root_guard();  // 容易忘記！
    // ...
});

// v2: 使用 spawn_with_gc!
let gc = Gc::new(42);
spawn_with_gc!(gc => |handle| async move {
    // handle 自動被追蹤
    println!("{}", *handle);
});
```

#### Step 3: 處理 escape 需求

```rust
// v2: 使用 EscapeableHandleScope
fn create_in_scope<'outer>(
    outer: &'outer HandleScope<'_>,
    tcb: &mut ThreadControlBlock,
) -> Handle<'outer, i32> {
    let escape_scope = EscapeableHandleScope::new(tcb);
    
    let gc = Gc::new(compute_value());
    let inner_handle = escape_scope.handle(&gc);
    
    escape_scope.escape(inner_handle)
}
```

### 9.2 Checklist

- [ ] 所有 `Gc::new()` 都有對應的 `HandleScope`
- [ ] 移除所有 `root_guard()` 呼叫，改用 `spawn_with_gc!`
- [ ] 確認沒有 Handle 逃逸出 scope (編譯器會報錯)
- [ ] 考慮是否需要 `EscapeableHandleScope`
- [ ] 測試在 Release mode 下的行為

---

## 10. 效能考量

### 10.1 基準測試預期

| 操作 | v1 (Conservative) | v2 (HandleScope) | 差異 |
|------|-------------------|------------------|------|
| Handle 分配 | N/A | O(1) bump pointer | 新開銷但極小 |
| Root 收集 | O(stack_size) | O(handle_count) | 顯著改善 |
| GC 掃描 | 不確定 (false positive) | 精確 | 改善 |
| 記憶體開銷 | 無 | ~256 handles/block | 可接受 |

### 10.2 最佳實踐

1. **儘早創建 HandleScope**: 避免頻繁創建/銷毀
2. **合理設定 block 大小**: 預設 256 適合大多數情況
3. **使用 SealedHandleScope**: 在效能敏感區域防止意外分配
4. **避免過深巢狀**: 過多層 scope 增加 escape 複雜度

---

## 11. 附錄

### 11.1 完整 API 總覽

```rust
// 核心類型
pub struct HandleScope<'env>;
pub struct Handle<'scope, T>;
pub struct EscapeableHandleScope<'env>;
pub struct MaybeHandle<'scope, T>;

// Async 支援
pub struct AsyncHandleScope;
pub struct AsyncHandle<T>;

// 輔助類型
pub struct LocalHandles;
pub struct HandleBlock;
pub struct HandleSlot;
pub struct HandleScopeData;

// Macros
macro_rules! spawn_with_gc;

// 函數
pub unsafe fn find_gc_box_from_ptr(
    heap: &LocalHeap,
    ptr: *const u8,
) -> Option<NonNull<GcBox<()>>>;
```

### 11.2 參考實作

| 概念 | V8 對應 | rudo-gc v2 |
|------|---------|------------|
| HandleScopeData | `HandleScopeData` | `HandleScopeData` |
| LocalHandles | `LocalHandles` | `LocalHandles` |
| HandleScope | `LocalHandleScope` | `HandleScope<'env>` |
| EscapableHandleScope | `EscapableHandleScope` | `EscapeableHandleScope<'env>` |
| Handle | `Handle<T>` / `DirectHandle<T>` | `Handle<'scope, T>` |

### 11.3 V8 原始碼參考

- `src/handles/local-handles.h:19-42` — LocalHandles
- `src/handles/local-handles.h:44-89` — LocalHandleScope
- `src/handles/handles.h:149-245` — Handle<T>
- `src/handles/handles.h:263-347` — HandleScope
- `src/handles/handles.h:378-599` — DirectHandle (CSS mode)

---

*文件結束*

**下一步**: 根據此規格實作 `crates/rudo-gc/src/handles/` 模組
