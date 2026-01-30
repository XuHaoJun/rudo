# Research: Tokio Async/Await Integration Patterns

**Feature**: 004-tokio-async-integration  
**Date**: 2026-01-30  
**Sources**: tokio-rs codebase at `/learn-projects/tokio-rs/`

## 1. Runtime Initialization Pattern

**Source**: `tokio-macros/src/entry.rs` (782 lines)

### Decision: Use runtime builder pattern for #[gc::main] macro

### Implementation Details

```rust
// Configuration parsing pattern
enum RuntimeFlavor {
    CurrentThread,
    Threaded,
    Local,
}

// Builder pattern for runtime creation
let rt = match config.flavor {
    RuntimeFlavor::CurrentThread | RuntimeFlavor::Local => {
        quote_spanned! {last_stmt_start_span=>
            Builder::new_current_thread()
        }
    }
    RuntimeFlavor::Threaded => quote_spanned! {last_stmt_start_span=>
        Builder::new_multi_thread()
    },
};

// Add configuration options
if let Some(v) = config.worker_threads {
    rt = quote_spanned! {last_stmt_start_span=> #rt.worker_threads(#v) };
}
if let Some(v) = config.start_paused {
    rt = quote_spanned! {last_stmt_start_span=> #rt.start_paused(#v) };
}

// block_on wrapper
return #rt
    .enable_all()
    .build()
    .expect("Failed building the Runtime")
    .block_on(#body_ident);
```

### Rationale

- **Extensibility**: Runtime flavor and worker threads are configurable
- **Error handling**: compile_error! for missing tokio_unstable flag
- **IDE support**: token_stream_with_error pattern provides partial expansion on error
- **Type safety**: Force typecheck without runtime overhead for non-never returns

### Alternatives Considered

| Alternative | Why Rejected |
|-------------|--------------|
| Direct tokio::spawn without runtime | Requires user to create runtime manually; GcRootSet may not be initialized |
| Runtime::current() | Only works within existing runtime; doesn't guarantee GcRootSet initialization |
| Global runtime handle | Less flexible; harder to test; violates tokio best practices |

## 2. Task Tracking Pattern

**Source**: `tokio-util/src/task/task_tracker.rs` (731 lines)

### Decision: Use atomic counting pattern for root tracking

### Implementation Details

```rust
struct TaskTrackerInner {
    // Lowest bit = closed state, rest = task count
    state: AtomicUsize,
    on_last_exit: Notify,
}

impl TaskTrackerInner {
    #[inline]
    fn add_task(&self) {
        self.state.fetch_add(2, Ordering::Relaxed);
    }

    #[inline]
    fn drop_task(&self) {
        let state = self.state.fetch_sub(2, Ordering::Release);
        // If this was the last task and we are closed:
        if state == 3 {
            self.notify_now();
        }
    }

    #[inline]
    fn set_closed(&self) -> bool {
        let state = self.state.fetch_or(1, Ordering::AcqRel);
        if state == 0 {
            self.notify_now();
        }
        (state & 1) == 0
    }
}
```

### Rationale

- **Performance**: Atomic operations are lock-free and cache-friendly
- **Memory efficiency**: Single atomic word instead of mutex + counter
- **Correctness**: Proper ordering (AcqRel) ensures synchronization
- **Wait-free**: Checking state doesn't require locking

### Ordering Analysis

| Operation | Ordering | Rationale |
|-----------|----------|-----------|
| fetch_add(2, Relaxed) | Relaxed | Count is approximate; no synchronization needed |
| fetch_sub(2, Release) | Release | Ensures task completion visible before counter decrement |
| fetch_or(1, AcqRel) | AcqRel | Both publishes closed state and acquires others' updates |
| load(Acquire) | Acquire | Sees all updates before release stores |

### Alternatives Considered

| Alternative | Why Rejected |
|-------------|--------------|
| Mutex<usize> | Higher overhead; requires OS lock even for uncontended case |
| RwLock<usize> | Same issues as Mutex; read-heavy pattern not needed |
| AtomicU64 with wider data | Overkill; usize is sufficient |

## 3. Spawn Pinning Pattern

**Source**: `tokio-util/src/task/spawn_pinned.rs` (446 lines)

### Decision: Use oneshot channel + guard pattern for gc::spawn wrapper

### Implementation Details

```rust
struct LocalPool {
    workers: Box<[LocalWorkerHandle]>,
}

impl LocalPool {
    fn spawn_pinned<F, Fut>(&self, create_task: F) -> JoinHandle<Fut::Output>
    where
        F: FnOnce() -> Fut,
        F: Send + 'static,
        Fut: Future + 'static,
        Fut::Output: Send + 'static,
    {
        let (sender, receiver) = oneshot::channel();
        let (worker, job_guard) = self.find_and_incr_least_burdened_worker();

        worker.runtime_handle.spawn(async move {
            let _job_guard = job_guard;  // Owned, dropped on completion

            // Abort handling
            let (abort_handle, abort_registration) = AbortHandle::new_pair();
            let _abort_guard = AbortGuard(abort_handle);

            // Send callback to LocalSet task
            let spawn_task = Box::new(move || {
                spawn_local(async move {
                    Abortable::new(create_task(), abort_registration).await
                });
            });

            // Wait for join handle and propagate result
            let join_handle = receiver.await?;
            join_handle.await
        })
    }
}

// Guard pattern for cleanup
struct JobCountGuard(Arc<AtomicUsize>);

impl Drop for JobCountGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
```

### Rationale

- **Ownership**: Guard is moved into spawned task, ensuring cleanup
- **Lifetime safety**: Guard dropped when task completes
- **Error propagation**: oneshot channel carries join handle back
- **Cancellation**: AbortHandle ensures cleanup on task cancellation

### Alternatives Considered

| Alternative | Why Rejected |
|-------------|--------------|
| Arc<GcRootGuard> | Reference counting overhead; guard ownership is clear without Arc |
| Reference counting | More complex; lifetime-based ownership is simpler in Rust |
| Callback pattern | Less ergonomic; future wrapper is more idiomatic |

## 4. Dirty Flag Pattern

### Decision: Use AtomicBool for root set dirty flag

### Implementation Details

```rust
pub struct GcRootSet {
    roots: Mutex<Vec<usize>>,
    count: AtomicUsize,
    dirty: AtomicBool,
}

impl GcRootSet {
    pub fn register(&self, ptr: usize) {
        let mut roots = self.roots.lock().unwrap();
        if !roots.contains(&ptr) {
            roots.push(ptr);
        }
        drop(roots);
        self.count.fetch_add(1, Ordering::AcqRel);
        self.dirty.store(true, Ordering::Release);
    }

    pub fn snapshot(&self) -> Vec<usize> {
        let roots = self.roots.lock().unwrap().clone();
        self.dirty.store(false, Ordering::Release);
        roots
    }
}
```

### Rationale

- **Efficiency**: Dirty flag allows GC to skip collection when roots unchanged
- **Simplicity**: Single boolean flag is easy to understand and verify
- **Performance**: No false collections when nothing changed

## 5. Proc-Macro Implementation Patterns

### From tokio-macros/src/entry.rs

```rust
// Token parsing pattern
type AttributeArgs = syn::punctuated::Punctuated<syn::Meta, syn::Token![,]>;

// Configuration building
fn build_config(
    input: &ItemFn,
    args: AttributeArgs,
    is_test: bool,
    rt_multi_thread: bool,
) -> Result<FinalConfig, syn::Error> {
    if input.sig.asyncness.is_none() {
        let msg = "the `async` keyword is missing from the function declaration";
        return Err(syn::Error::new_spanned(input.sig.fn_token, msg));
    }
    // ... parse attributes ...
}

// Error recovery for IDE support
fn token_stream_with_error(mut tokens: TokenStream, error: syn::Error) -> TokenStream {
    tokens.extend(error.into_compile_error());
    tokens
}
```

### Rationale

- **IDE support**: Partial expansion on error keeps completions working
- **Error messages**: syn::Error provides span information for better diagnostics
- **Attribute parsing**: Punctuated<Meta, Token![,]> handles comma-separated values

## 6. Summary of Decisions

| Pattern | Source | Decision |
|---------|--------|----------|
| Runtime initialization | entry.rs | Runtime builder with configurable flavor/threads |
| Task counting | task_tracker.rs | AtomicUsize with bit packing |
| Spawn wrapping | spawn_pinned.rs | Future wrapper + owned guard |
| Dirty flag | Custom | AtomicBool for root change detection |
| Proc-macros | entry.rs | Error recovery via token_stream_with_error |

All decisions align with:
- **Memory safety**: RAII guards ensure cleanup
- **Performance**: Lock-free atomics where possible
- **Cross-platform**: std::sync::atomic only
- **Rust idioms**: Future wrapper pattern, trait extension
