# Benchmarks

Measured on Oct 22, 2026 on a 4-core Windows laptop, release build. Reproduce with the commands below; numbers on your machine will differ.

## End to end (`dgf analyze`, all cores)

Wall clock including startup, file discovery, parsing, scoring and writing JSON. Best of 3.

| Code | Files | Size | Functions | Time |
| --- | ---: | ---: | ---: | ---: |
| This repository | 33 | – | 648 | 49 ms |
| npm (JavaScript, `node_modules/npm`) | 1,131 | 6.8 MB | 15,637 | 1.3 s |
| Rust crates in `~/.cargo/registry/src` | 1,782 | 39.4 MB | 42,396 | 5.6 s |

```powershell
Measure-Command { dgf analyze <folder> --no-config --format json -o out.json }
```

## One thread, files in memory

`examples/bench.rs` parses and scores files already read into memory, on one thread, so it isolates the engine from disk and parallelism:

```powershell
cargo run --release -p dog-food-core --example bench -- <folder> 3
```

| Code | Throughput |
| --- | ---: |
| Rust | 2.8 MB/s, 128 files/s |
| JavaScript | 2.6 MB/s, 440 files/s |

Tree-sitter parsing alone measures about 4 MB/s on the same machine, so parsing is roughly two thirds of the time and scoring the rest.

## A fix this benchmark found

The first measurement was 1.5 MB/s. Cognitive complexity asked every syntax node for its parent to detect `else` branches, and in tree-sitter `parent()` walks down from the root, so it is expensive on every node. Counting the `else` keyword token instead, and looking up parents only for the few `if` nodes, made the engine 1.9× faster and the end-to-end runs 1.7–2.2× faster with identical results.

## Not measured yet

- A comparison with ESLint's `complexity` rule on the same TypeScript code.
- Large C# code bases (Roslyn, ASP.NET Core).
- Linux and macOS.
