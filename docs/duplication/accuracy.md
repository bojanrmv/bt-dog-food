# Accuracy

[Duplication](README.md) › Accuracy

Two questions: does it find the copies (recall), and is what it reports real (precision)?

## Recall: the labelled corpus

[tests/corpus/csharp](../../crates/dog-food-dup/tests/corpus/csharp/README.md) holds C# written to be measured:
- copies of types 1 to 3;
- 16 groups of methods that do the same thing written differently (type 4);
- near misses that do different things;
- decoys that only look alike.

Its labels are checked by running the code on generated inputs.

```powershell
cargo run -p dog-food-dup --bin dgf-dup-bench                     # score dgf dup on it
cargo run -p dog-food-dup --bin dgf-dup-bench -- --report r.json  # score any tool's JSON
```

| Run | Types 1-3 | Type 4 | Wrong "equal" claims |
| --- | --- | --- | --- |
| syntactic engine alone | 14 of 15 | 1 of 47 | 0 |
| with `--semantic` | 14 of 15 | 47 of 47 | 0 |
| with `--confirm` | | all 17 groups confirmed by running | every near miss it can run told apart |

## Precision: real code, labelled by hand

Every finding of a scan of an open-source repository at a fixed commit, read and labelled *real* or *noise* ([tests/corpus/real](../../crates/dog-food-dup/tests/corpus/real/README.md)):

```powershell
dgf-dup-bench --real crates/dog-food-dup/tests/corpus/real/eshop.json --report eshop.json
```

| Code | Findings | Precision |
| --- | --- | --- |
| dotnet/eShop | 34 | 100% (77% before generated code, nested copies and configuration lists were left out) |
| a private ASP.NET Core backend, sample of 83 | 83 | 99%; all 15 type-4 claims right |

## Speed

| Code | Syntactic | With `--semantic` |
| --- | --- | --- |
| this repository (84 files) | under 1 s | |
| an ASP.NET Core backend (2,000 files, 25 projects) | about 3 s | about 25 s |

The two engines run at the same time. The C# engine binds one method at a time on every core, and skips before binding the methods whose syntax already shows they will be left out.
