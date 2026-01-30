# Procedural Macros Contract

**Contract**: #[gc::main] and #[gc::root] attribute macros
**Date**: 2026-01-30

## #[gc::main]

### Signature

```rust
#[proc_macro_attribute]
pub fn main(args: TokenStream, item: TokenStream) -> TokenStream
```

### Usage

```rust
#[gc::main]
async fn main() {
    // Code here runs with GcRootSet initialized
    // and tokio runtime active
}
```

### With Options

```rust
#[gc::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() {
    // Multi-threaded runtime with 4 worker threads
}

#[gc::main(flavor = "current_thread")]
async fn main() {
    // Single-threaded runtime
}
```

### Transformation

Input:
```rust
#[gc::main]
async fn main() {
    println!("Hello");
}
```

Output:
```rust
fn main() {
    use ::rudo_gc::tokio::GcRootSet;
    GcRootSet::global();
    let rt = ::tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed building the Runtime");
    rt.block_on(async {
        println!("Hello");
    })
}
```

### Contract Requirements

| Requirement | Description |
|-------------|-------------|
| Async function | Must be applied to `async fn` |
| No arguments | `main` function cannot accept arguments |
| Runtime created | Automatically creates tokio runtime |
| GcRootSet initialized | Calls `GcRootSet::global()` |
| Blocking wrap | Wraps body in `runtime.block_on()` |

### Options

| Option | Type | Default | Description |
|--------|------|---------|-------------|
| `flavor` | string | "multi_thread" | Runtime flavor: "multi_thread" or "current_thread" |
| `worker_threads` | integer | None | Number of worker threads (multi_thread only) |

### Error Cases

| Error | Message |
|-------|---------|
| Not async fn | "the `async` keyword is missing from the function declaration" |
| Main with args | "the main function cannot accept arguments" |
| Unknown option | "Unknown attribute {name} is specified; expected one of: `flavor`, `worker_threads`" |
| Invalid flavor | "No such runtime flavor `{s}`. The runtime flavors are `current_thread` and `multi_thread`." |

---

## #[gc::root]

### Signature

```rust
#[proc_macro_attribute]
pub fn root(args: TokenStream, item: TokenStream) -> TokenStream
```

### Usage

```rust
#[gc::main]
async fn main() {
    let gc = Gc::new(Data { value: 42 });

    #[gc::root]
    async {
        // All Gc pointers in this block are automatically guarded
        println!("{}", gc.value);
    };
}
```

### Transformation

Input:
```rust
#[gc::root]
async {
    println!("{}", gc.value);
}
```

Output:
```rust
{
    let _guard = ::rudo_gc::tokio::GcRootGuard::enter_scope();
    async {
        println!("{}", gc.value);
    }
}
```

### Contract Requirements

| Requirement | Description |
|-------------|-------------|
| Async block | Must be applied to `async { ... }` block |
| Automatic guard | Creates `GcRootGuard` at block entry |
| Guard dropped | Guard drops when block completes |
| Gc access | Gc pointers accessed in block are protected |

### Error Cases

| Error | Message |
|-------|---------|
| Not async block | Compile error (syn parse failure) |
| Nested #[gc::root] | Supported (each creates independent guard) |

---

## Derive Crate Export

```rust
// crates/rudo-gc-derive/src/lib.rs

mod main;
mod root;

pub use main::main;
pub use root::root;
```

## Feature Gating

Both macros are only available when the `tokio` feature is enabled:

```rust
// In rudo-gc-derive, macros are always available
// But they generate code that requires tokio feature

// User code using macros:
#[gc::main]
async fn main() {}

// When tokio feature is disabled, this produces compile error
// because GcRootSet is not defined
```
