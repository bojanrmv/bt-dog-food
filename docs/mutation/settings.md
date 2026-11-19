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
sandbox_dir = "~/dgf-sandbox"                          # default %TEMP%/dgf-mutate; see below
level = "standard"                                     # basic, standard (default), advanced
ignore_mutators = ["string"]                           # by name, see Mutators
ignore_methods = ["*Log*", "ToString", "ConfigureAwait", "Dispose"]
skip_plain_constructors = true   # constructors that only store their arguments are not mutated
thresholds = { high = 80, low = 60, break = 50 }       # below break: exit code 1
timeout_factor = 1.5                                   # a test run hangs after (startup + its tests) x 1.5
timeout_ms = 5000                                      # ... + 5 s
jobs = 3                                               # test runs and baseline workers at once, across all test projects (default: cores - 1)
build_jobs = 2                                         # dotnet build -m:2
low_priority = true                                    # builds and tests below normal priority
```

- **Test filters** use `dotnet test --filter` syntax. `"FILTER"` applies to every test project; `"Shop.Tests:FILTER"` applies to the test project named `Shop.Tests` only. As soon as one filter names a project, only the named projects run (plus every project if there is also a filter without a name). Several filters for one project match a test when any of them does. `--test-filter` can be repeated and replaces `test_filter` from the file.
- **Test projects** are the `.csproj` files that reference `Microsoft.NET.Test.Sdk`, or are Microsoft.Testing.Platform test applications (`EnableMSTestRunner`, `MSTest.Sdk`, the `MSTest`, `xunit.v3` or `TUnit` packages). A run uses those that `test_projects` allows, `exclude_test_projects` (and `--exclude-test-project`) does not exclude, and that reference a project with mutants, directly or through other projects. The globs match the path relative to the repository. A test project that a filter names runs even when the globs leave it out: `--test-filter "App.IntegrationTests:FullyQualifiedName~PricingTests"` brings back one excluded project for one run. Since a filter that names a project means only named projects run, name the others too (`--test-filter "App.Tests:FullyQualifiedName~."` runs all of `App.Tests`). When mutants no test reaches could be reached by a project the run left out, the report says which and gives these flags.
- The filter applies to every test run: the baseline that records reach, the packs and the solo runs. Mutants that only filtered-out tests reach are *no tests*.
- Filters, `test_projects` and `exclude_test_projects` are C# only. In a Microsoft.Testing.Platform test application, dog-food's worker understands filters made of `FullyQualifiedName=name` and `FullyQualifiedName~part`, joined with `|`.
- **`sandbox_dir`** is where the sandboxes, their run logs and the test workers dgf builds are kept, instead of `%TEMP%/dgf-mutate`. Use it where security software blocks or slows programs that build and run code in the temp folder: name one folder, and that folder is the only place `dgf mutate --run` builds and runs code, so it is the one to allow. It has to be outside the repository (a build inside it would find `Directory.Build.props`, `global.json` and `package.json` twice) and should be short, since build output goes deep and Windows paths end at 260 characters. A path is relative to `dgf.toml`, and `~/` is the home folder. The `DGF_SANDBOX_DIR` environment variable wins over the file: it is one machine's choice, where `dgf.toml` is the team's. A new folder starts empty, so the first run there builds everything once.

Every other `dgf.toml` section: [configuration](../configuration.md).
