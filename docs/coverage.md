# Coverage and CRAP

Complexity alone says a function has many paths. Coverage says whether your tests run them. Together they give the **CRAP score**, which points at code that is both complex and untested: the code most likely to break when someone changes it.

`dgf` never runs tests. Run them the way you already do with coverage turned on, then point `dgf` at the report.

## 1. Produce a coverage report

| Stack | Command | Report |
| --- | --- | --- |
| .NET (coverlet, built into `dotnet new xunit`/`nunit`/`mstest`) | `dotnet test --collect:"XPlat Code Coverage" --results-directory TestResults` | `TestResults/<guid>/coverage.cobertura.xml` |
| .NET with OpenCover output | `dotnet test --collect:"XPlat Code Coverage;Format=opencover"` | `TestResults/<guid>/coverage.opencover.xml` |
| Jest | `jest --coverage --coverageReporters=lcov` | `coverage/lcov.info` |
| Vitest | `vitest run --coverage --coverage.reporter=lcov` | `coverage/lcov.info` |
| c8 / nyc | `c8 --reporter=lcov npm test` | `coverage/lcov.info` |
| Rust | `cargo llvm-cov --lcov --output-path lcov.info` | `lcov.info` |

Any Cobertura XML, OpenCover XML or LCOV file works.

## 2. Analyze with it

```powershell
dgf analyze --coverage TestResults                 # a folder: every report inside is read
dgf analyze --coverage coverage/lcov.info
dgf analyze --coverage TestResults --format html -o dgf-report.html
```

`--coverage` can be repeated; reports covering the same file are added up (one per test project is fine). In a folder, files named `*.cobertura.xml`, `cobertura.xml`, `coverage.xml`, `*.opencover.xml`, `lcov.info` and `*.lcov` are used. To make it the default, add to `dgf.toml`:

```toml
[coverage]
paths = ["TestResults"]
```

Then a plain `dgf analyze` uses it. Remember to run the tests first, or the numbers are stale.

## 3. Read the result

```
COMPLEXITY  COGNITIVE  STATUS   COVERAGE    CRAP  FUNCTION          LOCATION
        11         15  FAIL           0%   132.0  Pricing.Shipping  src/Shop/Pricing.cs:20-34
         8          5  warn          91%     8.0  Pricing.Discount  src/Shop/Pricing.cs:5-18
```

`Pricing.Discount` is complex but well tested: CRAP 8, nothing to worry about. `Pricing.Shipping` is as complex and not tested at all: CRAP 132. Write tests for it first, then simplify it.

The HTML report adds a **complex and untested** chart: coverage across, complexity up, with the CRAP limit drawn as a curve. Everything in the shaded corner is above the limit.

## Limits

```toml
[limits]
crap_warn = 30   # default when coverage is loaded
crap_fail = 60   # optional: fail the run
```

The CRAP status combines with the complexity status: the worse of the two wins. Functions with `// dgf-ignore` or accepted by the baseline stay `ignored`.

## How functions and reports are matched

- Coverage is per line. A function's coverage is the share of its *instrumented* lines (the lines the report lists) that ran at least once. Functions with no instrumented line show `–`.
- Report paths are often absolute, or relative to a build folder. `dgf` matches a report file to a source file by the longest run of trailing path parts (`src/Shop/Pricing.cs`), so reports made on another machine or in CI still match. Separators and letter case do not matter.
- Cobertura `<source>` folders are applied when the report has exactly one.
