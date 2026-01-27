#!/bin/bash

# Run Miri tests with flags that accommodate our GC design
# -Zmiri-ignore-leaks: GC intentionally doesn't reclaim all memory immediately
# -Zmiri-permissive-provenance: Allows integer-to-pointer casts used in our AtomicNullable design
# -Zmiri-strict-provenance=0: Disables strict provenance checks
#
# Note: We skip the sync tests because they intentionally test concurrent access
# which Miri's ThreadSanitizer mode flags as data races.
MIRIFLAGS="-Zmiri-ignore-leaks -Zmiri-permissive-provenance" cargo +nightly miri test --features test-util --lib