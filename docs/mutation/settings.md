# Settings

[Mutation testing](README.md) › Settings

The same choices can live in `dgf.toml`, so every run (and every agent) uses them. Flags win over the file.

```toml
[mutate]
exclude = ["**/Migrations/", "**/Program.cs"]          # never mutated (.gitignore syntax)
test_filter = ["Shop.Tests:FullyQualifiedName~Pricing"] # see below
test_projects = ["**/*.Tests.csproj"]                  # test projects to use (default: all)
exclude_test_projects = ["**/*.Acceptance.Tests.csproj", "**/*.IntegrationTests.csproj"]
cache_dir = "D:/dgf-cache"                             # default .dgf/mutation-cache; ~/ is the home folder
max_minutes = 45
sandbox = "closure"                                    # or "full"
level = "standard"                                     # basic, standard (default), advanced
ignore_mutators = ["string"]                           # by name, see Mutators
ignore_methods = ["*Log*", "ToString", "ConfigureAwait", "Dispose"]
skip_plain_constructors = true   # constructors that only store their arguments are not mutated
thresholds = { high = 80, low = 60, break = 50 }       # below break: exit code 1
timeout_factor = 1.5                                   # a test run hangs after (startup + its tests) x 1.5
timeout_ms = 5000                                      # ... + 5 s
jobs = 3                                               # test runs at once (default: cores - 1)
build_jobs = 2                                         # dotnet build -m:2
low_priority = true                                    # builds and tests below normal priority
```

- **Test filters** use `dotnet test --filter` syntax. `"FILTER"` applies to every test project; `"Shop.Tests:FILTER"` applies to the test project named `Shop.Tests` only. As soon as one filter names a project, only the named projects run (plus every project if there is also a filter without a name). Several filters for one project match a test when any of them does. `--test-filter` can be repeated and replaces `test_filter` from the file.
- **Test projects** are the `.csproj` files that reference `Microsoft.NET.Test.Sdk`. A run uses those that `test_projects` allows, `exclude_test_projects` (and `--exclude-test-project`) does not exclude, and that reference a project with mutants, directly or through other projects. The globs match the path relative to the repository.
- The filter applies to every test run: the baseline that records reach, the packs and the solo runs. Mutants that only filtered-out tests reach are *no tests*.
- Filters, `test_projects` and `exclude_test_projects` are C# only.

Every other `dgf.toml` section: [configuration](../configuration.md).
