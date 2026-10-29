# On real projects

[Mutation testing](README.md) › On real projects

Measured on a real solution (a .NET 9 API with 3 xUnit test projects and 589 tests, plus an Angular 21 front end), on a 4-core laptop:

| Run | Mutants | Result | Time |
| --- | --- | --- | --- |
| C#: `dgf mutate <application project> --min-complexity 8 --run` (25 functions) | 233 | 53%: 119 killed, 41 survived, 63 no tests, 10 did not compile | 193 s: build 46 s, baseline 33 s, **38 test runs** for 160 reached mutants 118 s |
| The same, nothing changed | 233 | all from the cache | **0.6 s** |
| Angular: `dgf mutate .../pages/platform/reports --min-complexity 3 --run` (41 functions) | 343 | 12 killed, 325 no tests, 6 did not compile | 52 s: type check 9 s, baseline 22 s, 17 test runs 16 s |

Packing turned 160 reached C# mutants into 38 test runs. With one run per mutant, that part alone would take about 160 × 33 s ÷ 4 workers ≈ 22 minutes. The Angular run shows the other half of the story: most of that folder (a large component and a PDF service) has no tests at all, which dgf finds from one baseline run without testing a single mutant. A test that failed with no mutant (Angular's scaffolded "should render title") was set aside with a note instead of stopping the run.

On a feature branch of a similar solution (two test projects, 11 test classes chosen with a filter), on the same laptop under the load of a running dev environment: 404 mutants in the changed functions, 310 killed, 45 survived, 5 no tests, 1 did not compile and 43 ignored by settings, in **875 s** (build 69 s, baseline 41 s, 89 test runs 759 s). Before the checks for code that is hard to mutate, 14 mutants failed to compile and finding them took the build from about 70 s to 499 s.
