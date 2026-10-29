# Findings on real code, labelled by hand

The corpus next door measures recall on pairs built to be equal or not. This folder measures precision on code nobody wrote for a test: every finding of a scan of an open-source repository at a fixed commit, read and labelled by hand.

- `real`: the copies do one thing and could share code.
- `noise`: why not (generated code, a block found again inside a bigger copy, boilerplate such as dependency-injection registrations).

Findings are keyed by their class id, which stays while the copies stay in the same functions.

| Repository | Commit | Findings | Precision |
| --- | --- | --- | --- |
| [dotnet/eShop](https://github.com/dotnet/eShop) | `913acea` | 35 | 97% (34 real, 1 noise) |

The labels include the 9 findings that generated-file detection and nested-copy removal took out (77% before). If a change brings one back, it counts as noise again.

## Scoring a scan

```powershell
git clone https://github.com/dotnet/eShop; cd eShop; git checkout 913acea66708a59595351174cee9b29f37179d29
dgf-dup src --semantic --format json -o eshop.json
dgf-dup-bench --real path/to/tests/corpus/real/eshop.json --report eshop.json
```

The scan reads code and never builds or runs it (no restore, no `--confirm`). Without packages, the C# engine leaves out methods that need them. That lowers recall, not precision. The bench lists findings with no label: label them in the JSON so the file covers the scan.
