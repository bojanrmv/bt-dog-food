---
name: dgf-harden
description: Stage 4 of 4 (spec, build, clean, harden). Mutation testing on the lines a change touched: find the tests that run the code without checking it, and make them check. Use when asked to harden tests, to run mutation testing on a change, or after /dgf-clean, as the last step before review and QA.
argument-hint: "<name of the work, as in .dgf/work/<name>.md>"
---

# Harden: stage 4 of 4

Work: **$ARGUMENTS**

Coverage says a line ran. It does not say anything checked what the line did. Find the tests on
**this change's own lines** that watch without looking, and make them look. Never the whole file,
never the whole project. A normal run takes minutes; one heading for an hour has the wrong scope:
stop and fix the scope.

`dgf` runs mutants for C# (.NET 6 or newer; xUnit, NUnit, MSTest) and TypeScript or JavaScript
(Vitest, Jest, Angular's unit-test builder). The `dog-food` skill explains its output.

## Orient first

You have no memory of the previous stage. Read `.dgf/work/<name>.md`: the rules, and the last
entry of its `Log`. No clean entry there: stop and say `/dgf-clean` has to run first.

## 1. Scope: one second, nothing is built

```bash
dgf mutate --changes --scope lines --format agent      # lists functions and mutant counts
```

Read its `scope:` block first: the base it compares with, how far behind it the branch is, and
what `dgf.toml` left out.

- **0 mutants** (only tests, data shapes, configuration, wiring, markup, or a language `dgf` does
  not run): sign off now with `Gate: n/a, no logic to mutate (<files>)`.
- **A fix after an earlier harden:** add `--base <commit of that harden>` to every command below,
  so only the fix is mutated.

## 2. Run

```bash
dgf mutate --changes --scope lines --run --format agent
```

- **On a large solution, name the tests**, as you would by hand: `--test-filter
  "Project.Tests:FullyQualifiedName~FooTests"`, one per test project. Without it the first run
  executes every test of every project that reaches the change.
- **Every other setting comes from `dgf.toml`.** Do not override its excludes, jobs or budget.
- **A long run goes in the background**, with its output in a file. Do not print a whole run into
  your context.
- **A run uses most of the machine's cores.** Do not start a second one beside it unless the
  machine has room.
- **Another view of the same run** (html, json): the same command with `--cached` and another
  `--format`. It reads the cache and tests nothing.
- **After you sharpen tests:** the same command with `--rerun-survivors`. It tests only the
  survivors again.
- **Exit codes:** 0 done. 1 the score is below a threshold in `dgf.toml`. 2 a settings error, or
  the code does not build even without a mutant: stop and report `dgf`'s message. 4 the time
  budget ran out: run the same command once more, it continues from the cache; a second 4 means
  sign off with what was tested and carry the rest forward.

`dgf` builds and tests in a sandbox outside the repository. The working tree is never rewritten.

## 3. Read the survivors, once

| Outcome | What you do |
|---|---|
| killed, timeout, did not compile, ignored | nothing |
| **survived** (`tests_still_pass`) in the change's lines | sharpen an assertion in a test from its `tests:` line; `assert:` says what kind |
| **no tests** (`no_test_runs_this`) in the change's lines | write the test |
| survived or no tests in older code, or outside functions | one line in the carry, not chased |
| crashed the test run | run it once more; if it repeats, report it |

- A survivor marked **`boundary-risk`** (`<` against `<=` on a date or a time) is the
  off-by-one-day bug: it is never "equivalent" without a test that pins the boundary.
- Any other survivor you call **equivalent** (the mutant cannot change what the program does) is
  named with a one-line reason.

## Not yours

- **Production code is frozen.** Only test files change, so never a `// dgf-disable` comment
  either. A survivor that shows the production code is wrong is a defect: stop, report it, and
  name the stage that owns the fix (`/dgf-build`).
- No refactoring, no renaming, no complexity work.
- Tests that assert nothing are deleted when you meet them, or given an assertion.

## Gate

- Every survivor in the change's new or changed lines is killed, or named equivalent with a reason.
- The tests you ran and the spec's examples are green.
- Since the clean stage, **only test files changed** (`git diff --stat`).

## Sign off

Commit your work on the current branch unless the project's instructions say otherwise. Do not
push, merge or open a pull request unless asked. Then append to the `Log` of the work file and
report the same to the user:

```markdown
### harden · <date> · PASS | BLOCKED
Gate: <n> mutants on <files>: <k> killed, <e> equivalent, score <s>%
Changed: <test files>
Equivalent: <file:line> <mutant>, <reason>   (one line each)
Carried forward: <survivors in older code; mutants not tested when the budget ran out>
Next: review, then QA
```

Report first any **defect in production code** the mutants exposed, then the counts, then every
equivalent survivor with its reason.
