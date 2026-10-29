# Configuration: `dgf.toml`

`dgf` looks for `dgf.toml` in the current folder or a parent, the way cargo finds `Cargo.toml`. Paths in it are relative to the file. Every section is optional. Unknown keys are an error, so typos show up. `dgf init` writes the file with a comment on every setting.

An option on the command line wins over `dgf.toml`, which wins over the default.

```toml
[analyze]
exclude = ["dist/", "**/generated/", "tests/fixtures/"]   # .gitignore syntax
top = 20

[limits]
warn = 5            # above: "warn", still passes
fail = 6            # above: "FAIL", exit code 1
crap_warn = 30      # with coverage: CRAP above this is a warning (default 30)
# crap_fail = 60

[[limits.overrides]]   # looser limits for some paths; the last match wins
paths = ["src/Legacy/", "**/tests/"]
warn = 10
fail = 15

[changes]
base = "main"
scope = "lines"           # or "files"
fail_on = "new-or-worse"  # or "touched"
fail = 10                 # dgf changes only: hold new code to more than old code

[coverage]
paths = ["TestResults", "coverage/lcov.info"]   # used when --coverage is not given; dgf dup uses it too

[history]
dir = ".dgf/history"

[mutate]                  # every key: mutation/settings.md
exclude = ["**/Migrations/"]
exclude_test_projects = ["**/*.Acceptance.Tests.csproj"]
test_filter = ["Shop.Tests:FullyQualifiedName~Pricing"]
max_minutes = 45
ignore_methods = ["*Log*", "ToString"]   # calls whose arguments are never mutated
thresholds = { high = 80, low = 60, break = 50 }
```

## Defaults

| Setting | Without `dgf.toml` |
| --- | --- |
| limits | warn above 10, no fail limit |
| CRAP | warn above 30 (only with coverage) |
| excluded | `.gitignore` (even outside git) and generated files: `*.d.ts`, `*.g.cs`, `*.Designer.cs` |
| `changes` base | `main`, else `master` |

## In code

| Comment | Effect |
| --- | --- |
| `// dgf-ignore: reason` | above a function: it is reported but never fails |
| `// dgf-disable-once <mutator>: reason` | mutants of that kind on the next statement are not tested, and are reported as ignored with the reason ([turning mutants off](mutation/turning-off.md)) |
| `// dgf-disable` … `// dgf-restore` | the same, for everything in between |

To accept the functions already above the limits, so only new problems fail, use `dgf baseline` ([workflows](workflows.md#adopting-in-existing-code)).
