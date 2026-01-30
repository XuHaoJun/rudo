# gc::spawn Function Contract

**Contract**: Automatic root tracking spawn wrapper
**Date**: 2026-01-30

## Function Signature

```rust
#[cfg(feature = "tokio")]
pub async fn spawn<F, T>(future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
```

## Usage

```rust
use rudo_gc::tokio::spawn;

#[gc::main]
async fn main() {
    let gc = Gc::new(Data { value: 42 });

    // gc is automatically protected for the task's lifetime
    let handle = spawn(async move {
        println!("{}", gc.value);
        "done"
    });

    let result = handle.await.unwrap();
    assert_eq!(result, "done");
}
```

## Transformation

The `gc::spawn` function wraps the future with a `GcRootScope`:

```rust
pub async fn spawn<F, T>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let wrapped = GcRootScope::new(future);
    tokio::task::spawn(wrapped).await
}

struct GcRootScope<F> {
    future: F,
    _guard: GcRootGuard,
}

impl<F: Future> GcRootScope<F> {
    fn new(future: F) -> Self {
        Self {
            future,
            _guard: GcRootGuard::enter_scope(),
        }
    }
}

impl<F: Future> Future for GcRootScope<F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = unsafe { self.get_unchecked_mut() };
        let future = unsafe { Pin::new_unchecked(&mut this.future) };
        future.poll(cx)
    }
}
```

## Contract Requirements

| Requirement | Description |
|-------------|-------------|
| Future bound | `F: Future + Send + 'static` |
| Output bound | `F::Output: Send + 'static` |
| Automatic guard | Creates `GcRootGuard` when task spawns |
| Guard lifetime | Guard lives as long as the spawned task |
| Return type | Returns `JoinHandle<F::Output>` |

## Comparison with tokio::spawn

| Aspect | tokio::spawn | gc::spawn |
|--------|--------------|-----------|
| Automatic guard | No | Yes |
| Gc safety | Requires manual guard | Automatic |
| Bounds | Same | Same |
| Return type | JoinHandle | JoinHandle |

## Error Handling

| Scenario | Behavior |
|----------|----------|
| tokio feature disabled | Compile error (function not available) |
| Future not Send + 'static | Compile error |
| Task panic | Propagated via JoinHandle |
| Task cancellation | Aborted; guard dropped |

## Example: Multiple Spawns

```rust
#[gc::main]
async fn main() {
    let gc1 = Gc::new(Data { value: 1 });
    let gc2 = Gc::new(Data { value: 2 });

    // Each spawn creates independent guard
    let h1 = gc::spawn(async move {
        println!("Task 1: {}", gc1.value);
        gc1.value
    });

    let h2 = gc::spawn(async move {
        println!("Task 2: {}", gc2.value);
        gc2.value
    });

    let results = tokio::join!(h1, h2);
    assert_eq!(results, (1, 2));
}
```

## Performance Characteristics

| Operation | Complexity |
|-----------|------------|
| spawn() call | O(1) |
| Guard creation | O(1) |
| Guard drop | O(1) |
| Memory overhead | ~32 bytes per spawned task |

## Thread Safety

- Function is `Send + Sync`
- Guard uses atomic operations
- Safe to call from any tokio thread
- Safe to use with multi-threaded runtime
