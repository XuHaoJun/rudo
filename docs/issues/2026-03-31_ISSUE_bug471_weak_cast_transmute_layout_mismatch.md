# [Bug]: Weak::cast transmute between types with different sizes is UB

**Status:** Open
**Tags:** Not Verified

## 📊 威脅模型評估 (Threat Model Assessment)

| 評估指標 | 等級 | 說明 |
| :--- | :--- | :--- |
| **Likelihood (發生機率)** | Medium | Developers may cast between types of different sizes unknowingly |
| **Severity (嚴重程度)** | Critical | Undefined behavior - can read wrong amount of memory |
| **Reproducibility (復現難度)** | Medium | PoC would involve types with different sizes; existing tests demonstrate the issue |

---

## 🧩 受影響的組件與環境 (Affected Component & Environment)
- **Component:** `Weak::cast` in `ptr.rs`
- **OS / Architecture:** All
- **Rust Version:** 1.75+
- **rudo-gc Version:** Current

---

## 📝 問題描述 (Description)

### 預期行為 (Expected Behavior)
`Weak::cast<U>()` should only be safe when `GcBox<T>` and `GcBox<U>` have the same size, ensuring memory safety when the upgraded `Gc<U>` accesses the `value` field.

### 實際行為 (Actual Behavior)
`Weak::cast()` uses `std::mem::transmute` to convert `NonNull<GcBox<T>>` to `NonNull<GcBox<U>>` without verifying that the two types have the same size. This is undefined behavior when `size_of::<GcBox<T>>() != size_of::<GcBox<U>>()`.

The function's safety comment states:
> The caller must ensure that T and U have the same layout

But the existing tests demonstrate casting between types of different sizes:
- `Weak<Inner>` (where `Inner` is `{ value: i32 }` - 4 bytes) cast to `Weak<u8>` (1 byte)
- `Weak<Outer>` (containing `Inner`) cast to `Weak<Inner>`

---

## 🔬 根本原因分析 (Root Cause Analysis)

**File:** `crates/rudo-gc/src/ptr.rs:2597-2605`

```rust
pub fn cast<U: Trace + 'static>(self) -> Weak<U> {
    let ptr = self.ptr.load(Ordering::Acquire);
    std::mem::forget(self);
    let atomic_ptr = ptr.as_option().map_or_else(AtomicNullable::null, |p| {
        let cast_p: NonNull<GcBox<U>> = unsafe { std::mem::transmute(p) };
        AtomicNullable::new(cast_p)
    });
    Weak { ptr: atomic_ptr }
}
```

`GcBox<T>` is defined as:
```rust
#[repr(C)]
pub struct GcBox<T: Trace + ?Sized> {
    ref_count: AtomicUsize,
    weak_count: AtomicUsize,
    drop_fn: unsafe fn(*mut u8),
    trace_fn: unsafe fn(*const u8, &mut GcVisitor),
    is_dropping: AtomicUsize,
    generation: AtomicU32,
    value: T,  // <-- Size depends on T
}
```

When `T` and `U` have different sizes, `size_of::<GcBox<T>>() != size_of::<GcBox<U>>()`. The `transmute` between `NonNull<GcBox<T>>` and `NonNull<GcBox<U>>` is undefined behavior per Rust's transmute semantics: "Transmuting between types of different sizes is undefined behavior."

When `Weak::<U>::upgrade()` succeeds and creates `Gc<U>`, it accesses `value: U`. If the actual allocation was sized for `T` (larger), this reads insufficient memory; if `T` was smaller, it reads beyond the allocated region.

---

## 💣 重現步驟 / 概念驗證 (Steps to Reproduce / PoC)

The existing tests in `tests/weak_cast.rs` already demonstrate the issue:

```rust
#[test]
fn test_weak_cast_basic() {
    let gc = Gc::new(Inner { value: 42 });  // Inner is { value: i32 } - 4 bytes
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    // Casts Weak<Inner> (4 bytes value) to Weak<u8> (1 byte value)
    // GcBox<Inner> has sizeof(GcBoxHeader) + 4
    // GcBox<u8> has sizeof(GcBoxHeader) + 1
    // These are different sizes - transmute is UB!
    let weak_cast: Weak<u8> = weak.cast::<u8>();

    assert!(weak_cast.is_alive());
    assert!(weak_cast.may_be_valid());
}

#[test]
fn test_weak_cast_struct_to_u8() {
    let gc = Gc::new(Inner { value: 123 });  // Inner = { value: i32 } = 4 bytes
    let weak: Weak<Inner> = Gc::downgrade(&gc);

    // Same issue - Inner is 4 bytes, u8 is 1 byte
    let weak_bytes: Weak<u8> = weak.cast::<u8>();

    assert!(weak_bytes.is_alive());
    let upgraded = weak_bytes.upgrade();
    assert!(upgraded.is_some());  // If this upgrade returns Some and accesses value, UB occurs
}
```

**Verification approach:**
1. Compile with `RUSTFLAGS="-Z sanitizer=address"` and run tests
2. Inspect generated assembly to verify memory access patterns differ
3. Use Miri to detect undefined behavior

---

## 🛠️ 建議修復方案 (Suggested Fix / Remediation)

**Option 1: Compile-time safety via trait (preferred)**
Add a marker trait to verify size equivalence:
```rust
pub unsafe trait LayoutCompatible<T> {}
unsafe impl<T: Trace + 'static> LayoutCompatible<T> for T {}

pub fn cast<U: Trace + 'static + LayoutCompatible<U>>(&self) -> Weak<U>
```

**Option 2: Runtime assert with panic**
Add a static assertion in `cast`:
```rust
pub fn cast<U: Trace + 'static>(self) -> Weak<U> {
    // Compile-time check using const generics would be better
    assert!(std::mem::size_of::<GcBox<T>>() == std::mem::size_of::<GcBox<U>>(),
            "Weak::cast requires GcBox<T> and GcBox<U> to have the same size");
    // ... rest of implementation
}
```

**Option 3: Remove the unsafe transmute (breaking change)**
Use `NonNull::as_ptr()` and reconstruct, but this requires careful handling since `GcBox<T>` and `GcBox<U>` are different types.

---

## 🗣️ 內部討論紀錄 (Internal Discussion Record)

**R. Kent Dybvig (GC 架構觀點):**
The cast operation fundamentally reinterprets the type parameter. For correct GC operation, the physical allocation size must match what the type parameter suggests. If a `Weak<u8>` thinks it points to a 1-byte value but the actual allocation has a 4-byte value, incorrect memory access occurs during trace/drop. The existing tests in `weak_cast.rs` that cast between types of different sizes (Inner/i32 to u8) would be incorrect even if transmute weren't UB - they'd cause memory corruption.

**Rustacean (Soundness 觀點):**
This is a clear violation of Rust's transmute safety requirements. The transmute between `NonNull<GcBox<T>>` and `NonNull<GcBox<U>>` where sizes differ is undefined behavior per Rust's memory model. The function's safety comment ("caller must ensure same layout") is insufficient - the API should enforce this constraint or use a safe alternative. The existing tests that pass differently-sized types demonstrate the unsafety is not just theoretical.

**Geohot (Exploit 觀點):**
An attacker could exploit this by:
1. Creating a `Weak<LargeStruct>` pointing to a large allocation
2. Casting to `Weak<SmallStruct>`
3. When the weak is upgraded and the value accessed, if the upgrade returns the original pointer but code assumes smaller size, out-of-bounds read could leak adjacent heap data
4. This could be leveraged for heap introspection attacks if the GC allocator has predictable layout