
# Testing
I replicated the 2 tests in the same file but in the `with_nom` module. I also added one more test (which is just a copy of the `..._right` with an additional left path segment added) in the same module, so to run the relevant tests, run

```
> cargo test -p tezos_messages with_nom
```

# Benchmark
I added a benchmark in `tezos/messages/benches/nom_comparison.rs`. To run it execute

```
> cargo bench -p tezos_messages nom_vs_serde
```

On my machine the benchmark gives `2.0739 us` for the new nom implementation and `40.564 us` for the serde parsing.

# Bounding paths
My proposal to prevent stack overflow when parsing paths, would be to introduce a `RecursionGuard` struct

```rust
struct RecursionGuard<T> {
    inner: T,
    depth: usize,
    max_depth: usize,
}

fn path(input: RecursionGuard<&[u8]>) -> IResult<RecursionGuard<&[u8]>, RecursionGuard<Path>> {
    todo!()
}
```

where `T` would be either input or output of the parsing function depending on context. This would allows us to monitor recursion depth and error out at a predetermined `max_depth`.

This however proved difficult to combine with some of `nom`'s combinators.

# Notes
Keep in mind that this implementation completely ignores the `BinaryDataCache` of each struct, it's likely possible to support this field using the [`consumed'](https://docs.rs/nom/6.0.1/nom/combinator/fn.consumed.html) combinator.
