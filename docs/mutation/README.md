# Mutation testing

Coverage tells you which lines your tests *run*. Mutation testing tells you whether they would *notice* if those lines were wrong: it changes the code on purpose (`>` becomes `>=`, a condition becomes `false`), runs the tests, and expects at least one to fail. A mutant no test catches is a gap in the tests.

```powershell
dgf mutate --changes                       # which functions, how many mutants (instant, nothing runs)
dgf mutate --changes --run                 # build, test, list what the tests miss
```

![The mutation dashboard: the score, each mutant no test caught, results by function](../images/mutation.png)

C# (xUnit, NUnit, MSTest) and TypeScript/JavaScript (Vitest, Jest, Angular's unit-test builder) are run. Rust functions are listed but not run.

## Why it is fast

The cost of mutation testing is **test runs × mutants**. Compiling every mutant into one build and using coverage to choose tests are the usual answers; dog-food goes further on both factors:

1. **Fewer mutants.** Mutate functions, not projects: only what a branch changed, what has a high CRAP score, or what is over the complexity limits. dgf already knows all three.
2. **Fewer, cheaper test runs.** Record which test reaches which mutant, then *pack* many mutants into one test run (each test carries at most one mutant of its own), run only the tests that reach them, run packs in parallel, and never re-test a function whose code and tests have not changed.

The tests always run on the real .NET runtime (or Node for TypeScript and JavaScript), so a killed mutant means exactly what it means in any other tool.

## Quick start

```powershell
dgf mutate --changes --run                 # what this branch changed: the everyday use
dgf mutate src/Orders --run                # one area
dgf mutate --run --jobs 8 --fail-under 60  # 8 test runs at a time; exit 1 below 60%
dgf mutate --run --format agent            # survivors only, for an AI assistant to fix
dgf mutate --run --format html -o mutation.html   # the mutation page
dgf mutate --run --no-cache                # test everything again, ignoring cached results
dgf mutate --run --level basic             # fewer mutators: operators, blocks, ??
dgf mutate --run --rerun-survivors         # after adding tests: survivors again, kills from the cache
dgf mutate --changes --scope lines --run   # only the mutants on lines this branch changed
dgf mutate --run --format mutation-report -o mutation.json   # the mutation-testing-elements JSON
dgf analyze                                # MUTATION column from the cache

# On a large solution: only the tests that matter, and a time limit
dgf mutate --changes --run \
  --exclude-test-project "**/*.Acceptance.Tests.csproj" \
  --test-filter "Shop.Tests:FullyQualifiedName~PricingTests" \
  --max-minutes 45
```

## What a result means

A mutant is **killed** when a test fails with it on, and **survived** when every test that reaches it still passes: a gap to close with a test. Mutants no test reaches are **no tests**. The score is the share detected, per function and overall. Every outcome is in [running and reports](running.md#what-a-run-shows).

## What it needs

- **C#:** test projects that reference `Microsoft.NET.Test.Sdk` (xUnit, NUnit, MSTest) and run on VSTest (`dotnet test` without Microsoft Testing Platform), and `dotnet` on the PATH. Which test projects run is described under [Settings](settings.md).
- **TypeScript/JavaScript:** a `package.json` above the mutated files with `vitest` or `jest`, its `node_modules` installed, and Node 22.3 or newer. In an Angular workspace whose project uses `@angular/build:unit-test`, dgf runs `ng test <project>` instead, and type-checks the mutants with `tsc` against the project's `tsconfig.spec.json` first (Angular's builder rejects code that does not type-check, plain Vitest and Jest do not check types).
- Tests that fail with no mutant active are left out with a note; the run stops when no test runs at all, or when half of them or more fail.
- Rust functions are listed by `dgf mutate` but not run.
- The sandbox is a folder in `%TEMP%/dgf-mutate` (see [How it works](how-it-works.md)); your working tree is never touched. Projects that use files outside the repository folder will not build there.
- Results are cached in `.dgf/mutation-cache/` in the repository unless `cache_dir` says otherwise (add `.dgf/mutation-cache/` to `.gitignore`; the rest of `.dgf/` is meant to be committed).
- Several `dgf mutate` runs can work at the same time, from one repository or from several worktrees: each takes a sandbox of its own, and they share the cache safely.

## In this section

| Page | What it covers |
| --- | --- |
| [Choosing what to mutate](choosing.md) | functions by branch, lines, path or complexity; listing mutants |
| [Mutators](mutators.md) | every mutator per level and language, what is never made, code that is hard to mutate |
| [Turning mutants off](turning-off.md) | `dgf-disable` comments, `ignore_methods`, date boundaries |
| [Running and reports](running.md) | a run's output, the report formats, thresholds, the time budget, exit codes |
| [Settings](settings.md) | `[mutate]` in `dgf.toml`, test filters and test projects |
| [How it works](how-it-works.md) | sandbox, one build, reach per test, packing, solo runs, the cache |
| [On real projects](real-projects.md) | measured runs on a .NET API and an Angular front end |

Every option: `dgf mutate --help`, or [the command reference](../commands.md#dgf-mutate).
