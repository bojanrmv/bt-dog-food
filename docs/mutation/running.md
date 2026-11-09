# Running and reports

[Mutation testing](README.md) › Running and reports

```powershell
dgf mutate --changes --run
```

## What a run shows

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
| crashed the test run | the test host ended before the tests it was asked for reported (dog-food's worker says so itself), or no test reported at all | left out |
| ignored | a `dgf-disable` comment, `ignore_methods`, `skip_plain_constructors` or `[ExcludeFromCodeCoverage]`; never tested | left out |
| not tested | `--max-minutes` ran out first; or it could not be tested: no test runner above the file, its test run could not start, every test that reaches it fails without any mutant, or no test that ran reaches it and a skipped test project uses its code (the reason is given with it) | left out |

Score = detected ÷ (detected + survived + no tests), per function and overall.

The run first runs the tests once without mutants. When half of them or more fail, it stops: fix them, or leave them out with a test filter. Fewer failing tests are set aside, and only the others decide about mutants. For TypeScript and JavaScript, the test files with failing tests run once more first: a test that passes then is kept, and the report's notes name it, since a test that fails only now and then makes its kills less certain. Tests that still fail are named in the report's notes too. A mutant that only failing tests reach is listed as not tested, not as having no tests: it has tests, which cannot judge it.

## Reports

- `--format table` (default), `json`, `html` and `agent` as for every command. `--format mutation-report` writes the mutation-testing-elements JSON (schema version 2, checked in the tests against the schema): each file with its source and mutants, the mutator names above, 1-based locations with an exclusive end, and the format's statuses (a mutant `--max-minutes` left untested is `Pending`). Any viewer of that format reads it.
- `thresholds` (`high` 80 and `low` 60 by default) set the bands the table names and the HTML page colors; `break` (or `--fail-under`) fails the run below it, with exit code 1.
- In `--format agent` every survivor comes with the tests that reach it and their files (where a sharper assertion goes, or "none reaches this") and what kind of assertion would catch it: the boundary value for `<` → `<=`, the exact text for a string, the effect of a removed statement.
- After adding tests, `--rerun-survivors` tests the survivors again even when the cache says nothing changed; kills still come from the cache. The report lists each survivor it tested again and its outcome now (`tested again (--rerun-survivors): ...`).
- With `--changes`, functions outside the branch's diff that a changed test file reaches get a score line of their own; the mutation score and `--fail-under` are about the branch's own functions.

The HTML page is described in [the dashboards](../dashboards.md#mutation).

## Time budget

`--max-minutes N` (or `max_minutes`) stops a run cleanly when the time is up: test runs still going are stopped, nothing new starts, every outcome so far goes into the cache, and the report lists each untested mutant with the step that did not finish (building, the baseline test run, or its own test runs) and, for a mutant some of whose tests ran, how many of them passed: it is not a survivor until every test that reaches it has. A mutant whose own test run could not start (a run folder that could not be cleared, a worker that would not start) is listed as not tested with the error, outside the budget's count, and the run goes on without it. The exit code is then **4**. Running the same command again continues where it stopped, since the finished mutants come from the cache.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Done, and the score is not below `--fail-under` (or `break` in `thresholds`) |
| 1 | The score is below `--fail-under` (or `break`); a run where no mutant counted never fails |
| 2 | Usage or settings error, or the sandbox does not build without mutants (a C# test project that is all that fails is left out instead; see how it works) |
| 3 | A path does not exist or a file could not be written |
| 4 | `--max-minutes` ran out; the report shows what was and was not tested |
