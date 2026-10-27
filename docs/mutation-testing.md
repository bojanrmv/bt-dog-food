# Mutation testing in dog-food

Coverage tells you which lines your tests *run*. Mutation testing tells you whether they would *notice* if those lines were wrong: it changes the code on purpose (`>` becomes `>=`, a condition becomes `false`), runs the tests, and expects at least one to fail. A mutant no test catches is a gap in the tests.

The cost of mutation testing is **test runs × mutants**. Compiling every mutant into one build and using coverage to choose tests are the usual answers; dog-food goes further on both factors:

1. **Fewer mutants.** Mutate functions, not projects: only what a branch changed, what has a high CRAP score, or what is over the complexity limits. dgf already knows all three.
2. **Fewer, cheaper test runs.** Record which test reaches which mutant, then *pack* many mutants into one test run (each test carries at most one mutant of its own), run only the tests that reach them, run packs in parallel, and never re-test a function whose code and tests have not changed.

The tests always run on the real .NET runtime (or Node for TypeScript and JavaScript), so a killed mutant means exactly what it means in any other tool.

## Phases

| Phase | What | State |
| --- | --- | --- |
| 1 | Choose functions (`--changes`, complexity) and generate mutants with tree-sitter | **done**: `dgf mutate` |
| 2 | C# runner: sandbox copy with mutant switches, one `dotnet build`, mutation score per function | **done**: `dgf mutate --run` |
| 3 | Per-test reach recording, mutant packing (many mutants per test run), only the tests that reach a mutant, parallel runs | **done** |
| 4 | Cache by function and test hash; mutation score in `dgf analyze` reports and an HTML mutation page | **done** |
| 5 | TypeScript/JavaScript runner (Vitest and Jest) | **done** |
| 6 | Speed and safety on large solutions: test filters and test project exclusion, a cache per mutant that worktrees can share, a sandbox with only the projects the tests need, a time budget, runs side by side | **done** |
| 7 | Mutators: the C# and TypeScript sets with `--level`, regex patterns, mutants in expressions that declare variables, `dgf-disable` comments with a required reason, `ignore_methods`, `[ExcludeFromCodeCoverage]` | **done** |
| 8 | Settings and reports: thresholds, the mutation-testing-elements JSON report, a timeout from each test's own time, OpenCover input for `dgf analyze`; and for agents and shared machines: `--rerun-survivors`, the tests behind each survivor, the date-boundary rule, `--scope lines`, functions whose tests changed, `--no-mix`, `--jobs`/`--build-jobs`/`--low-priority`, sandboxes shared by worktrees | **done** |

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

## Settings

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

## Phase 1: choosing functions and generating mutants

```powershell
dgf mutate                                 # every function in the repository
dgf mutate src/Orders --min-complexity 4   # only functions with complexity 4 or more
dgf mutate --changes                       # only functions this branch changed (vs main)
dgf mutate --changes --show                # list every mutant
dgf mutate --format agent                  # compact, for AI assistants
```

```
MUTANTS  COMPLEXITY  WHY       FUNCTION          LOCATION
     15           4  selected  Pricing.Shipping  src/Shop/Pricing.cs:19-30
                               21:13  weight <= 0  ->  false   (condition)
                               21:13  weight <= 0  ->  !(weight <= 0)   (negate)
                               21:20  <=  ->  >   (relational)
                               21:20  <=  ->  <   (relational)
                               23:13  throw new ArgumentOutOfRangeException(nameof(weight));  ->  ;   (statement, ignored: a guard clause; callers validate weight first (dgf-disable, line 23))
                               25:19  >=  ->  <   (relational)
                               26:9  { return 0; }  ->  {return default;}   (block)
                               29:16  weight > 10  ->  true   (conditional)
                               ...
```

### Mutators

`--level` (or `level`) picks `basic`, `standard` (the default) or `advanced`. In TypeScript and JavaScript, `basic` keeps only the operators and block removal. Names in `ignore_mutators` and in `dgf-disable` comments are the ones in the left column, in any case.

| Mutator | Level | C# | TypeScript |
| --- | --- | --- | --- |
| `arithmetic` | basic | `+` ↔ `-`, `*` ↔ `/`, `%` → `*`; not on strings | the same |
| `relational` | basic | `<` → `>`, `<=`; `<=` → `>`, `<`; `>` → `<`, `>=`; `>=` → `<`, `>`; also in patterns (`is > 5`) | `<` → `<=`, `>=`; `<=` → `<`, `>`; `>` → `>=`, `<=`; `>=` → `>`, `<` |
| `equality` | basic | `==` ↔ `!=`; `x is P` ↔ `x is not P` | `==` ↔ `!=`, `===` ↔ `!==` |
| `logical` | basic | `&&` ↔ `\|\|`; pattern `and` ↔ `or`; `a ^ b` → `a == b` | `&&` ↔ `\|\|`, `??` → `&&` |
| `bitwise` | basic | `&` ↔ `\|`, `<<` → `>>`, `>>>` and back; `a ^ b` → `~(a ^ b)` | – |
| `block` | basic | a block → `{}`, or `{return default;}` where the method returns a value; only when no other mutant is inside it | a block → `{}` |
| `nullcoalescing` | basic | `a ?? b` → `b`, `a`, `b ?? a` | – |
| `statement` | standard | `Save();`, `return;`, `break;`, `continue;`, `goto`, `throw`, `yield` → `;` | – |
| `call` | standard | – | `save();` and `throw x;` → `;`, when nothing else inside is mutated |
| `boolean` | standard | `true` ↔ `false` | the same |
| `negation` | standard | `!x` → `x` | the same |
| `negate` | standard | `if (c)`, `while (c)` → `!(c)`, unless `c` is `!x`, `==`, `!=` or `is` | – |
| `condition` | standard | an `if` condition → `false` | an `if` or `?:` test → `true` and `false`; a loop test → `false`; a boolean expression → `true` and `false`; a `case` loses its statements |
| `conditional` | standard | `c ? a : b` → `true ? a : b`, `false ? a : b` | – |
| `unary` | standard | `-x` ↔ `+x`, `~x` → `x` | the same |
| `update` | standard | `x++` ↔ `x--`, `++x` ↔ `--x` | the same |
| `assignment` | standard | `+=` ↔ `-=`, `*=` ↔ `/=`, `%=` → `*=`, `&=`, `\|=`, `^=`, shifts, `??=` → `=` | `+=` ↔ `-=`, `&&=` ↔ `\|\|=`, `??=` → `&&=` and more |
| `checked` | standard | `checked(e)` → `e` | – |
| `linq` | standard | `First` ↔ `FirstOrDefault`, `Any` ↔ `All`, `OrderBy` ↔ `OrderByDescending`, `Skip` ↔ `Take`, `Sum` → `Max`, `Count` → `Sum` and more pairs; not to a method that needs an argument the call lacks | – |
| `string` | standard | `"a"` → `""`, `""` → `"dgf was here!"`, `$"..."` → `$""`, `string.Empty`, `string.IsNullOrEmpty(x)` → `x != null`, `x != ""` | `"a"` → `""`, `` `a` `` → ` `` ` `` |
| `initializer` | standard | `new T { A = 1 }`, `new List<int> { 1 }`, `new int[] { 1 }`, `int[] a = { 1 }` → `{ }` | – |
| `array`, `object` | standard | – | `[a, b]` → `[]`, `[]` → `["dgf was here"]`, `new Array(3)` → `new Array()`; `{ a: 1 }` → `{}` |
| `arrow`, `optional`, `method` | standard | – | `() => a` → `() => undefined`; `a?.b` → `a.b`; `s.trim()` → `s`, `startsWith` ↔ `endsWith` and more |
| `math`, `stringmethod`, `collection` | advanced | `Math.Floor` ↔ `Math.Ceiling` and the other `Math` swaps; `StartsWith` ↔ `EndsWith`, `ToUpper` ↔ `ToLower`, `s.Trim()` → `""`; `[a, b]` → `[]` | – |
| `regex` | advanced (TypeScript: standard) | the pattern of `new Regex("...")`, and of `Regex.IsMatch(s, "...")` and the other static methods too: sixteen kinds of change, such as `^` and `$` removed, `[a-z]` → `[^a-z]`, `[b-z]`, `[\w\W]`, `\d` → `\D`, `d`, `[\d\D]`, `a+` → `a`, `a{0,}`, `a{2,}`, `a+?`, `(a)` → `(?:a)`, `(?=a)` → `(?!a)` | a regex literal or `new RegExp("...")`: the six that suit JavaScript: `^` and `$` removed, class and `\d` negation, quantifier removal, lookaround negation |

Each failed `dotnet build` costs a full rebuild, so dgf decides before building which mutants could not compile:

- **Not made, because they cannot compile:** `+` and `+=` where one side is a `string` variable of the same function; `x is T v` → `x is not T v` and `!(c)` when the declared `v` is used in the branch; `&&` ↔ `||` when the left side declares a variable the right side uses; removing the last `throw` of a function that returns a value; removing a block in a method with `out` parameters; a `?.` chain is never cut in the middle (that would also change what the unmutated code does).
- **Not made, because they cannot change behaviour:** removing a `return;` or `continue;` that is the last statement of its function or loop.
- **Regex patterns** are parsed by dgf; a pattern it does not understand gets no regex mutants, and a mutated pattern that no longer parses is dropped, since `new Regex` would reject it (a group made non-capturing while `\1` refers to it). An interpolated pattern gets none.
- **Copies that keep their lines:** a mutant whose switch has to copy a string spanning lines or an `#if` line keeps the copy's line breaks between `#line` directives, so every line keeps its number for the compiler. In TypeScript, a line break in a template string is written `\n` in the copy; a tagged template (`` String.raw`...` ``) spanning lines cannot be copied, and a mutant that would copy one is not made.
- **`#if` with branches that only parse one at a time** (an expression body in each branch): only the first branch is mutated.
- **Also made:** `condition` for C# (an `if` condition → `false`), and regex mutants for `Regex`'s static methods.
- In TypeScript, the sandbox adds `// @ts-nocheck` to mutated files, so a mutant only has to be valid syntax.

**Expressions that declare a variable** (`o is Order order`, `out var n`) are mutated too. A switch normally copies the mutated expression next to the original, which would declare the variable twice; such a mutant switches the whole statement instead (`{if (switch) {mutated statement} else {original statement}}`), or stays inside the expression when its mutated copy no longer declares the variable (a condition → `false`). When the variable is used after the statement, neither works, and the mutant is not made. A mutant that only changes a condition's value (`condition`, `conditional`, `negate`, `negation`) is kept even then, when every variable the condition declares is an `out` argument that is always evaluated: it becomes `(c) && !switch`, `(c) || switch` or `(c) != switch`, so `!map.TryGetValue(k, out var v)` still runs once and `v` is assigned as before. The condition is still evaluated, so a side effect in it (the `TryGetValue` itself) still happens.

**Code that is hard to mutate.** Some code has mutants that cannot compile. dgf keeps the function and leaves out only those mutants, mostly before building:

- `out var` in conditions (`if (!int.TryParse(s, out var n)) return;`), as above;
- pattern variables used after the `if` (`if (o is not string t) return 0; return t.Length;`): turning that condition to `false` leaves `t` unassigned, which no tool can compile, so those mutants are not made and the rest of the function is mutated;
- a local assigned in `try` whose `catch` returns or throws, and an `if` that returns when a `TryGet` fails: the block or jump whose removal would leave the variable unassigned is not mutated, the rest of the function is;
- `out` parameters assigned inside `try`/`catch`;
- `while (true)` with a `return` inside, in a function that returns a value: the loop condition is not mutated (`false` would make the end of the function reachable without a value);
- `??` whose left side is a nullable value (`decimal? price ?? 0`): the right side alone is kept, the left side alone is not made (it would be `decimal?` where a `decimal` is needed).

When a mutant slips through and the compiler reports "use of unassigned local variable" (CS0165) or "not all code paths return a value" (CS0161), the error points at a use or at the end of the function, not at the mutant: every mutant of that function becomes a suspect and is put back half at a time until the culprit is found.

**New C#.** The C# grammar (tree-sitter-c-sharp 0.23) predates C# 14 and 15. Before mutating, dgf rewrites what it does not know into code of the same length that it does, so every byte keeps its place: `closed` classes, labeled `break` and `continue`, null-conditional assignment (`a?.B = c`), `extension` blocks and file-based app directives (`#:package`). Only the parse sees the rewrite; the mutants are made and compiled on the original text. `Modern.cs` in the fixtures uses C# 12 to 15 and is compiled with every mutant switched in when a .NET 11 SDK is installed.

Not mutated at all:

- functions marked `// dgf-ignore`;
- test code: `*.test.*`, `*.spec.*`, `*Tests.cs`, `*Test.cs`, and anything in a `test`, `tests`, `__tests__`, `spec` or `*.Tests` folder;
- places that only take constants: `const`, attributes, parameter defaults (C#), enum members, `case` and pattern constants; in TypeScript, types, imports, decorators, enums and `x as T`.

Each mutant is one byte range and its replacement (`dog_food_core::mutate::mutate`), so it can be applied alone (`Mutant::apply`) or compiled in next to the others (`dog_food_core::mutate::schemata`). `crates/dog-food-core/tests/mutants/` holds a C# and a TypeScript file that state, line by line, which mutants each line must get; the C# one is also compiled with every mutant switched in.

### Turning mutants off

Mutants that are not tested stay in every report as **ignored**, with the reason, and do not count in the score.

```csharp
// dgf-disable-once relational: the sign is all that matters here
return a < 0 ? -1 : 1;

var b = a > 1; // dgf-disable-once relational, string: same; on the statement this comment ends

// dgf-disable-next-line arithmetic: unit conversion, checked by the end-to-end tests
var cents = euros * 100;

// dgf-disable all: generated mapping, tested through the API
...
// dgf-restore all
```

- `dgf-disable-once` covers the next statement, or the statement the comment ends when it follows code on the same line. `dgf-disable-next-line` covers the next line. `dgf-disable` covers everything up to `dgf-restore` with the same names (or the end of the file); `dgf-restore relational` ends only that mutator's part.
- The reason after `:` is required: a comment without one, or with an unknown mutator name, stops the run with exit code 2 and says where.
- Every comment is listed in every report format with its reason and how many mutants it turned off.
- `ignore_methods`: `*` stays within one part of a dotted name, any qualifier may come first (`*Log*` matches `_logger.LogInformation`, and also `Login`), generic arguments are optional, `Type.ctor` names a constructor. A mutant is ignored when it is inside such a call (its arguments, lambdas in them, the call it is chained from), or is a statement, assignment, `await` or declaration whose value is such a call, or a block that holds only such calls.
- Members (and types) marked `[ExcludeFromCodeCoverage]` are ignored.

## Running the mutants

First run on a small C# project (2 functions, 22 mutants, xUnit, 4-core laptop):

```
SCORE  KILLED  SURVIVED  NO TESTS  TIMEOUT  FUNCTION          LOCATION
  38%       3         5         0        0  Pricing.Discount  src/Shop/Pricing.cs:5-18
   0%       0         0        14        0  Pricing.Shipping  src/Shop/Pricing.cs:20-34

Not caught by any test (add or sharpen a test for each):
  src/Shop/Pricing.cs:9:13  member && total > 100  ->  false   (condition, tests still pass)  in Pricing.Discount
  src/Shop/Pricing.cs:16:18  >  ->  >=   (relational, tests still pass)  in Pricing.Discount
  ...
mutation score 14% · 3 killed, 5 survived, 14 no tests, 0 timeout, 0 did not compile; 0 from the cache
```

`Pricing.Shipping` has no tests at all: the reach recording found that without running a single test per mutant. The five survivors in `Pricing.Discount` are real gaps: no test has a member above 100, and none hits the 12% cap. After adding tests for `Shipping`, the 22 mutants took **11 test runs** instead of 22 (packing), and the second run with nothing changed took **0.1 s** (all 22 from the cache).

| Outcome | Meaning | In the score |
| --- | --- | --- |
| killed | a test failed with the mutant on | detected |
| timeout | the test run took longer than (startup + the time its tests took without mutants) × 1.5 + 5 s (often an endless loop) | detected |
| survived | every test that reaches it passed: a gap | not detected |
| no tests | no test reaches this code | not detected |
| did not compile | removed before testing | left out |
| crashed the test run | the test run ended before the tests it was meant to run reported | left out |
| ignored | a `dgf-disable` comment, `ignore_methods` or `[ExcludeFromCodeCoverage]`; never tested | left out |
| not tested | `--max-minutes` ran out first | left out |

Score = detected ÷ (detected + survived + no tests), per function and overall.

The run first runs the tests once without mutants. When half of them or more fail, it stops: fix them, or leave them out with a test filter. Fewer failing tests are set aside, and only the others decide about mutants.

### Reports

- `--format table` (default), `json`, `html` and `agent` as for every command. `--format mutation-report` writes the mutation-testing-elements JSON (schema version 2, checked in the tests against the schema): each file with its source and mutants, the mutator names above, 1-based locations with an exclusive end, and the format's statuses (a mutant `--max-minutes` left untested is `Pending`). Any viewer of that format reads it.
- `thresholds` (`high` 80 and `low` 60 by default) set the bands the table names and the HTML page colors; `break` (or `--fail-under`) fails the run below it, with exit code 1.
- In `--format agent` every survivor comes with the tests that reach it and their files (where a sharper assertion goes, or "none reaches this") and what kind of assertion would catch it: the boundary value for `<` → `<=`, the exact text for a string, the effect of a removed statement.
- After adding tests, `--rerun-survivors` tests the survivors again even when the cache says nothing changed; kills still come from the cache.

### Date boundaries

A comparison of dates or times (`today <= plan.LastDay`, `x is > DateOnly.MinValue`) is marked **boundary-risk** in every report: a surviving `<=` → `<` there is the off-by-one-day bug. Without types, dgf calls an operand a date when the file declares it as `DateOnly`, `DateTime`, `DateTimeOffset` or `TimeOnly` (a parameter, local, field or property, or a `var` set from one), when its name says so (`EndDate`, `LastDay`, `CreatedAt`, `ExpiresOn`, `today`, `UtcNow`), or when it builds one (`DateOnly.FromDateTime(...)`). Such a mutant may only be turned off by a `dgf-disable` comment whose reason names the test that pins the boundary, `// dgf-disable-once relational: the last day counts, test: PlanTests.EndsOnLastDay`; dgf checks that the test exists in the test code and stops with exit code 2 otherwise.

### Time budget

`--max-minutes N` (or `max_minutes`) stops a run cleanly when the time is up: test runs still going are stopped, nothing new starts, every outcome so far goes into the cache, and the report lists each untested mutant with the step that did not finish (building, the baseline test run, or its own test runs). The exit code is then **4**. Running the same command again continues where it stopped, since the finished mutants come from the cache.

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Done, and the score is not below `--fail-under` (or `break` in `thresholds`) |
| 1 | The score is below `--fail-under` (or `break`); a run where no mutant counted never fails |
| 2 | Usage or settings error, or the sandbox does not build without mutants |
| 3 | A path does not exist or a file could not be written |
| 4 | `--max-minutes` ran out; the report shows what was and was not tested |

### On real projects

Measured on a real solution (a .NET 9 API with 3 xUnit test projects and 589 tests, plus an Angular 21 front end), on a 4-core laptop:

| Run | Mutants | Result | Time |
| --- | --- | --- | --- |
| C#: `dgf mutate <application project> --min-complexity 8 --run` (25 functions) | 233 | 53%: 119 killed, 41 survived, 63 no tests, 10 did not compile | 193 s: build 46 s, baseline 33 s, **38 test runs** for 160 reached mutants 118 s |
| The same, nothing changed | 233 | all from the cache | **0.6 s** |
| Angular: `dgf mutate .../pages/platform/reports --min-complexity 3 --run` (41 functions) | 343 | 12 killed, 325 no tests, 6 did not compile | 52 s: type check 9 s, baseline 22 s, 17 test runs 16 s |

Packing turned 160 reached C# mutants into 38 test runs. With one run per mutant, that part alone would take about 160 × 33 s ÷ 4 workers ≈ 22 minutes. The Angular run shows the other half of the story: most of that folder (a large component and a PDF service) has no tests at all, which dgf finds from one baseline run without testing a single mutant. A test that failed with no mutant (Angular's scaffolded "should render title") was set aside with a note instead of stopping the run.

On a feature branch of a similar solution (two test projects, 11 test classes chosen with a filter), on the same laptop under the load of a running dev environment: 404 mutants in the changed functions, 310 killed, 45 survived, 5 no tests, 1 did not compile and 43 ignored by settings, in **875 s** (build 69 s, baseline 41 s, 89 test runs 759 s). Before the checks for code that is hard to mutate, 14 mutants failed to compile and finding them took the build from about 70 s to 499 s.

### What it needs

- **C#:** test projects that reference `Microsoft.NET.Test.Sdk` (xUnit, NUnit, MSTest) and run on VSTest (`dotnet test` without Microsoft Testing Platform), and `dotnet` on the PATH. Which test projects run is described under [Settings](#settings).
- **TypeScript/JavaScript:** a `package.json` above the mutated files with `vitest` or `jest`, its `node_modules` installed, and Node 22.3 or newer. In an Angular workspace whose project uses `@angular/build:unit-test`, dgf runs `ng test <project>` instead, and type-checks the mutants with `tsc` against the project's `tsconfig.spec.json` first (Angular's builder rejects code that does not type-check, plain Vitest and Jest do not check types).
- Tests that fail with no mutant active are left out with a note; the run stops when no test runs at all, or when half of them or more fail.
- Rust functions are listed by `dgf mutate` but not run.
- The sandbox is a folder in `%TEMP%/dgf-mutate` (see [How it works](#how-it-works)); your working tree is never touched. Projects that use files outside the repository folder will not build there.
- Results are cached in `.dgf/mutation-cache/` in the repository unless `cache_dir` says otherwise (add `.dgf/mutation-cache/` to `.gitignore`; the rest of `.dgf/` is meant to be committed).
- Several `dgf mutate` runs can work at the same time, from one repository or from several worktrees: each takes a sandbox of its own, and they share the cache safely.

## How it works

1. **Sandbox.** A folder in `%TEMP%/dgf-mutate`, named after the repository and a hash of its path. Every worktree of a repository uses the same sandboxes (dgf asks git for the main checkout), so the first run in a new worktree brings a built sandbox up to date instead of building everything; a file whose size matches but whose time differs (a fresh checkout) is compared by content and left alone when it is the same, so the build stays incremental. It holds only what the tests need: for C#, the chosen test projects and every project they reference (each project folder without `bin` and `obj`), the files in every folder above them up to the repository root (where `Directory.Build.props`, `Directory.Packages.props`, `global.json` and `NuGet.config` live) with any `.config` folder there, and files that project, `.props` and `.targets` files import or include from elsewhere (`..\build\Shared.props`). For TypeScript and JavaScript, the package folder with the test runner, and the files above it. `sandbox = "full"` copies the whole repository instead, and dgf does that by itself, with a note, when the smaller copy does not build.

   The sandbox is kept between runs and brought up to date file by file: new and changed files are copied, deleted ones removed, and the build output stays, so the next build only compiles what changed. Links and NTFS junctions in the repository, and every `node_modules` folder, are linked again in the sandbox, never copied; removing the sandbox removes the links, not what they point at. A run locks its sandbox; a second run at the same time takes the next one (`-2`, `-3`, ...).

   Only the files with chosen functions are rewritten. Every mutated expression becomes a switch, nested where expressions nest: `a > b` turns into `(DgfMutants.Is(17) ? (a >= b) : (a > b))`. Every line keeps its number: line breaks in the mutated copy become spaces, or, in C# when the copy holds a string spanning lines or an `#if`, stay between `#line` directives. C# gets a small `DgfMutants.g.cs` in each mutated project; TypeScript and JavaScript get a `__dgfIs` function appended at the end of the file (function declarations are hoisted, so no line moves).
2. **One build.** C#: `dotnet build` of a solution with the chosen test projects, with analyzers off (source generators still run), warnings not treated as errors, and no NuGet audit: these slow a build down or fail it for reasons that have nothing to do with the mutants. Angular: `tsc --noEmit` on the spec tsconfig. Vitest and Jest need no build: they strip types on load.

   Mutants that do not compile (for example `+` on a `DateTime`, or `&&` → `||` that breaks TypeScript's narrowing) are found from the compiler's error lines. A mutant on an error's line is marked *did not compile* and removed. An error on a line with several mutants, or with none, makes them, or every mutant of that function, suspects: they are taken out together and put back half at a time until the culprit is found, so the others are still tested. Errors that point at a use or at the end of a function rather than at their cause (C# "use of unassigned local variable", "not all code paths return a value", TypeScript "used before being assigned") always make the whole function suspect. Suspects of different functions are searched for side by side, in the same builds. After the first build, only the projects with mutants are rebuilt (`--no-dependencies`: what they reference was built a moment ago) until they compile; a last build of everything the tests need follows. `--build-jobs N` (or `build_jobs`) passes `-m:N` to every build, and `--low-priority` runs dgf and everything it starts below normal priority (MSBuild's reused build nodes get `-lowPriority`).
3. **Reach, per test.** One test run with no mutant active and recording on; for C#, the test projects run side by side. Every `Is(id)` call writes "test, mutant" once. The switch knows the running test: in .NET through a small in-process data collector that dgf builds once and adds to the run settings (it sets AppDomain data `DgfTest` around each test); in Node from `expect.getState().currentTestName`. Mutants no test reaches are *no tests* without running anything.
4. **Packing.** A test run can try many mutants at once if each test carries at most one: dgf writes a plan file of "test → mutant id" lines and the switch turns on only the planned mutant for the running test. Packing runs in two rounds. First, each mutant gets one test: the quickest test that reaches it, spread so that no test carries many mutants (each mutant already given to a test counts as half a second more), which keeps the packs few. Most mutants that are killed at all are killed by that test, so most never run with the rest. Then the survivors run with every other test that reaches them. In each round, mutants are placed first-fit, the most-reached first, into packs of at most 200 tests; only those tests run (for `dotnet test`, by name in a filter that goes into the run settings, so its length does not matter; for Vitest/Jest, only the test files that hold them). A failing planned test kills its mutant. Packs run in parallel (`--jobs`, default: cores - 1, so one is left for the rest of the machine), each with its own results folder. `--no-mix` gives every mutant a test run of its own instead.

   A test run counts as hanging, and its mutants as *timeout*, after (startup + the time its tests took without mutants) × `timeout_factor` (1.5) + `timeout_ms` (5000). Each test's time comes from the baseline run (the .NET collector times every test; Vitest and Jest report it), and startup is what each test project's run took besides its tests. On Windows the limit is checked against processor time as well: a test run that is past it but has used less processor time than the limit (because the machine is busy, or the run is at `--low-priority`) gets more time, up to ten times the limit, while a run that keeps a processor busy, as an endless loop does, is stopped at the limit. dgf also confirms a timeout before it counts: a mutant whose own run timed out runs once more with four times the time (and at least 30 s more), and only a second timeout makes it *timeout*. On a loaded laptop the tight first limit is often missed by runs that are only slow; without the second run, a mutant no test catches can be reported as caught.
5. **Solo runs** for what packs cannot decide: mutants reached outside any test (static initialisers, module load), and packs that ran over the time limit. A solo run turns one mutant on for every test that reaches it, or for every chosen test (with its filter) when it is reached outside tests.
6. **Cache.** One entry per mutant, in `.dgf/mutation-cache/mutants.json` (or `cache_dir`). Entries are keyed by the file's path in the repository, the function's name and the hash of its text, never by branch or folder, so one `cache_dir` can serve every clone and worktree. An outcome is reused while the function's text and the build settings (project files in the closure, `Directory.Build.*`, `Directory.Packages.props`, `global.json`, `NuGet.config`; for Node, `package.json`, lock files and test-runner configs) are unchanged, and:
   - *killed*: the source files of the tests that killed it are unchanged (dgf finds a test's file from the test adapter, or else from the class that declares the test);
   - *survived*, *no tests*, *timeout*: no file of the chosen test projects (or test files of the package) changed, and the filters are the same, since any new or changed test could kill it;
   - *did not compile*: nothing more.

   The cache also records the test files that reach each mutant. When a branch changes a test file, `--changes` adds the functions whose mutants that file reached last time (reason `tests`); the rules above then decide what is tested again.

   The cache does not notice a change to other production code that makes a killing test miss the mutant; `--no-cache` tests everything again (and saves the new results). Runs that finish at the same time merge their results under a file lock, and the file is replaced in one step, so a reader never sees half of it. Entries unused for 30 days are dropped. `dgf analyze` reads the same cache to fill the MUTATION column in every report format.
7. **Report.** Table, JSON, agent, the mutation-testing-elements report and an HTML page (score, survivors, a bar per function, every mutant with an outcome filter, every `dgf-disable` comment).

Why this is fast:

- **Few mutants:** functions, not projects; `--changes` usually means tens of mutants, not thousands.
- **One build** for all mutants, and never more than one sandbox per language.
- **Packing:** with per-test reach, the number of test runs is about *max mutants a single test reaches*, not *number of mutants*: 22 mutants became 11 runs on the small sample.
- **One test first:** a mutant runs with all its tests only when its first test does not kill it.
- **Only reaching tests run** in each pack (C#: by name; Node: their test files).
- **Only the tests you choose run at all:** filters and excluded test projects apply to the baseline too, which is where unscoped mutation runs spend most of their time.
- **Small, lasting sandbox:** only the projects the tests need are copied, and the build output stays between runs.
- **Cache:** a killed mutant is never run again while its function, its killing tests and the build settings are unchanged.
