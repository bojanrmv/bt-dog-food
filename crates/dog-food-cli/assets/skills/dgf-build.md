---
name: dgf-build
description: Stage 2 of 4 (spec, build, clean, harden). Make the rules of a spec true, with tests, and nothing more. Use when asked to implement a user story or a spec written by /dgf-spec, or to build the work named in .dgf/work/<name>.md.
argument-hint: "<name of the work, as in .dgf/work/<name>.md>"
---

# Build: stage 2 of 4

Work: **$ARGUMENTS**

You make the spec true. Speed here, polish in the next stage: `/dgf-clean` exists so that you do
not have to be precious, and `/dgf-harden` so that you do not have to prove every assertion.

## Orient first

You have no memory of the previous stage. Read `.dgf/work/<name>.md` before anything else: the
job, the guardrails, the rules, and the last entry of its `Log`. No such file, or its
`Open questions` are not empty: stop and say `/dgf-spec` has to run first.

## Yours

- The behaviour the rules describe, one rule at a time: get its example passing, move on.
- A test for every rule and example in the spec, and unit tests for the code you write.
- Code that can be tested: file, network, clock and database access behind small seams.

## Not yours

- **The spec is frozen.** A rule that is wrong, missing or impossible is a stage 1 defect: stop,
  say which rule and why, and send it back to `/dgf-spec`. Do not build your own reading of it.
- No refactoring for shape, no hunting duplication, no mutation testing. Those are the next two
  stages; do not do their job badly on the way past.
- No cleanup of code you did not need to touch.
- Nothing the spec lists as out of scope, however close it is.

## How

- Follow the nearest existing code for patterns, names and layout. The project's conventions beat
  your preferences.
- Tests before or after the code, whichever is faster for you. The gate is what is covered, not
  the order.
- **Run only what your change can break**: the tests that cover the files you touched, the
  projects that depend on them. Never the whole suite in the inner loop.
- **Keep build and test output out of your context**: send it to a file and read only the
  failures.

## Gate

1. **The contract.** Every rule and example in the spec has a test, and all of them pass. If the
   project has executable specs for this work, those pass too, unchanged.
2. **Coverage of what you touched.** Run the covering tests with a coverage report, then:

   ```bash
   dgf analyze <files you touched> --coverage <report or folder> --format agent
   ```

   The project's coverage floor applies if it has one. Without one, every function you added is
   run by a test, and each branch you left uncovered is named in the report with its reason.
   Uncovered lines are your remaining test list: write those tests, do not explain the gap away.
3. **The build is clean** for the projects your change affects: compiler, linter and analyzer
   findings in files you touched are yours; findings elsewhere are not.
4. **Nothing became unshippable:**

   ```bash
   dgf changes --format agent
   ```

   Exit code 1 (a new or worse function above the `fail` limit) stops you here: split it now.
   Warnings do not: they are the Cleaner's starting list.

Never add a `// dgf-ignore`, change the baseline or raise a limit to pass.

## Sign off

Commit your work on the current branch unless the project's instructions say otherwise. Do not
push or open a pull request unless asked. Then append to the `Log` of the work file and report the
same to the user:

```markdown
### build · <date> · PASS | BLOCKED
Gate: <n>/<n> rules with a passing test; coverage of touched files <line>% line, <branch>% branch;
  dgf changes: <its first line>
Changed: <files>
Assumed: <decisions you made that neither the spec nor the user did>
Carried forward: <what the Cleaner should know: a seam left rough, a method you know is too long,
  a copy you made on purpose to keep the change small>
Next: /dgf-clean <name>
```

Report done only with the numbers. "Should be covered" is not a number.
