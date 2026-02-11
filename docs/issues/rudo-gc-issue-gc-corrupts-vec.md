# rudo-gc GC Collection Corrupts Vec<Gc<T>> Contents

## Summary

`rudo-gc` version 0.8.2 causes memory corruption when storing `Gc<T>` pointers in a `Vec`. After GC collection, the `Vec` contents appear to be corrupted - the `Gc<T>` pointers themselves are valid (same addresses), but the data they point to has been overwritten/corrupted.

**This is a critical memory safety bug that causes use-after-free or memory corruption.**

## Environment

- **Platform**: Linux x86_64 (Arch Linux)
- **Rust**: stable-x86_64-unknown-linux-gnu
- **rudo-gc**: 0.8.2 (crates.io)
- **Rvue**: GitHub repo (workspace setup)

## Error Message

```
thread 'main' panicked at .../rudo-gc-0.8.2/src/ptr.rs:1179:18:
misaligned pointer dereference: address must be a multiple of 0x8 but is 0x7f6d75696465
thread caused non-unwinding panic. aborting.
```

The panic occurs in `Gc::deref()` when trying to dereference a corrupted pointer.

## Minimal Reproduction Case

The following minimal Rust code reproduces the issue. Note that **simple standalone tests do NOT reproduce it** - it requires the specific context of Rvue's widget system.

```rust
// Minimal reproduction - this works fine in isolation
use rudo_gc::{Gc, Trace};

#[derive(Clone)]
struct TestItem {
    id: u32,
}

unsafe impl Trace for TestItem {
    fn trace(&self, _visitor: &mut impl rudo_gc::Visitor) {}
}

fn main() {
    let mut items: Vec<Gc<TestItem>> = Vec::with_capacity(10);

    for i in 0..10 {
        let item = Gc::new(TestItem { id: i as u32 });
        items.push(Gc::clone(&item));
    }

    println!("Before GC:");
    for (idx, item) in items.iter().enumerate() {
        println!("items[{}] = {}", idx, item.id);
    }

    rudo_gc::collect();

    println!("After GC:");
    for (idx, item) in items.iter().enumerate() {
        println!("items[{}] = {}", idx, item.id);  // Still correct here!
    }
}
```

**This test passes** - it does NOT reproduce the corruption.

### Reproduction in Rvue Context

The corruption only occurs in the specific context of Rvue's `For` widget:

```rust
// From crates/rvue/src/widgets/for_loop.rs

fn build(self, ctx: &mut BuildContext) -> Self::State {
    let initial_items = self.items.get();
    let initial_count = initial_items.len();

    // Create a Vec to store child components
    let mut child_components: Vec<Gc<Component>> = Vec::with_capacity(initial_count);

    // Create components and push to Vec
    for item in initial_items.iter() {
        let view = with_current_ctx(ctx.id_counter, || view_fn(item.clone()));
        let child_component = view.into_component();

        eprintln!("[DEBUG] Creating child component_id={}", child_component.id);
        child_components.push(child_component);  // PUSH: Correct id here

        eprintln!("[DEBUG] After push, vec[{}].id = {}",
                  child_components.len() - 1,
                  child_components[child_components.len() - 1].id);  // CORRECT
    }

    // Iterate and check - CORRUPTION HAPPENS HERE
    for (idx, comp) in child_components.iter().enumerate() {
        eprintln!("[DEBUG] Reading vec[{}].id = {}", idx, comp.id);  // CORRUPTED!
    }

    rudo_gc::collect();

    // Vec contents are now corrupted
}
```

## Debug Output (From Rvue)

```
[DEBUG] Creating child component_id=7, addr=0x7f8dc880d828
[DEBUG] After push, vec[0].id = 7, addr=0x7f8dc880d828
[DEBUG] Creating child component_id=11, addr=0x7f8dc8809828
[DEBUG] After push, vec[1].id = 11, addr=0x7f8dc8809828
[DEBUG] Creating child component_id=15, addr=0x7f8dc7a76828
[DEBUG] After push, vec[2].id = 15, addr=0x7f8dc7a76828
[DEBUG] Creating child component_id=19, addr=0x7f8dc7a72828
[DEBUG] After push, vec[3].id = 19, addr=0x7f8dc7a72828
...
[DEBUG] Components created
[DEBUG] Vec contents after iteration:
[DEBUG]   vec[0].id = 7, addr=0x7f8dc880d828      // Correct
[DEBUG]   vec[1].id = 42, addr=0x7f8dc8809828     // CORRUPTED: was 11, now 42
[DEBUG]   vec[2].id = 46, addr=0x7f8dc7a76828    // CORRUPTED: was 15, now 46
[DEBUG]   vec[3].id = 19, addr=0x7f8dc7a72828     // Correct (inconsistent pattern)
```

## Key Observations

1. **Memory addresses are correct**: The `Gc<T>` pointers at addresses like `0x7f8dc8809828` are still valid
2. **Component data is corrupted**: The `id` field (first field in Component struct) has been overwritten
3. **Corruption pattern is inconsistent**: Some indices are correct, some are corrupted
4. **Happens without GC**: The corruption occurs even when `rudo_gc::collect()` is called manually at specific points
5. **Not Vec reallocation**: Capacity is pre-allocated, so no reallocation should occur

## Component Structure

```rust
// From crates/rvue/src/component.rs
pub struct Component {
    pub id: ComponentId,              // Offset 0 - gets corrupted
    pub component_type: ComponentType,
    pub children: GcCell<Vec<Gc<Component>>>,  // Contains Gc pointers
    pub parent: GcCell<Option<Gc<Component>>>,
    pub effects: GcCell<Vec<Gc<Effect>>>,
    pub properties: GcCell<PropertyMap>,
    pub is_dirty: AtomicBool,
    // ... more fields
}
```

The `id` field is at offset 0, so any memory corruption at the start of the GcBox will overwrite it.

## Root Cause Hypothesis

The corruption appears to be caused by:

1. **Incremental marking**: rudo-gc uses incremental marking with SATB barriers
2. **Write barrier corruption**: The generational/incremental write barriers may be writing to wrong memory locations
3. **GcBox layout**: The BiBOP (Big Bag of Pages) memory layout may have alignment issues when GcBox contains nested Gc pointers

## Impact

This bug makes it impossible to:
1. Use `For` widget with dynamic lists
2. Store `Gc<Component>` in any Vec that might trigger GC
3. Build any non-trivial Rvue applications

## Reproduction Steps

```bash
cd /home/noah/Desktop/rvue
cargo run --bin hackernews-example
```

The crash should occur immediately during application startup when the `For` widget builds its child components.

## Related Files

- `/home/noah/Desktop/rvue/crates/rvue/src/widgets/for_loop.rs` - For widget with debug output
- `/home/noah/Desktop/rvue/crates/rvue/src/component.rs` - Component structure
- `/home/noah/Desktop/rvue/crates/rvue/tests/gc_vec_test.rs` - Test that passes (doesn't reproduce)

## Suggested Investigation

1. **Check write barrier implementation**: Verify barriers are writing to correct addresses
2. **Verify GcBox layout**: Ensure BiBOP allocation maintains proper alignment
3. **Test with Miri**: Run with `MIRIFLAGS=-Zmiri-tag-gc=1` to detect UB
4. **Check incremental marking**: Verify object relocation doesn't corrupt GcBox headers
5. **Isolate in rudo-gc**: Create a minimal test case that specifically targets the marking/tracing behavior

## Contact

Open issues at: https://github.com/anomalyco/rudo-gc/issues

For questions, please provide:
1. Steps to reproduce
2. Platform and Rust version
3. rudo-gc version
4. Debug output with `RUST_BACKTRACE=1`
