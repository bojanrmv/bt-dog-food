---
name: dgf-clean
description: Stage 3 of 4 (spec, build, clean, harden). Reshape freshly built code without changing what it does: split complex functions, unite copies, fix names and boundaries, measured with dgf before and after. Use when asked to clean up or refactor a finished change, to reduce its complexity, CRAP or duplication, or after /dgf-build.
argument-hint: "<name of the work, as in .dgf/work/<name>.md>"
---

# Clean: stage 3 of 4

Work: **$ARGUMENTS**

Behaviour is finished and correct. Your job is to make it survivable.
**You may not change what the code does, only how it is shaped.**

The `dog-food` skill explains every column `dgf` prints.

## Orient first

You have no memory of the previous stage. Read `.dgf/work/<name>.md`: the guardrails, and the last
entry of its `Log`, where the builder said what it left rough. No build entry there: stop and say
`/dgf-build` has to run first.

## Yours

- CRAP: `cyclomatic² × (1 − coverage)³ + cyclomatic`. Two levers: split the function, or cover it.
- Names of functions, variables, files, tests and helpers. Rename freely when intent gets clearer.
- Duplication, dead code, stale comments, parameters passed through for nothing.
- Functions and files that mix unrelated jobs.
- Boundaries: the rules of the business do not depend on the database, the network or a framework.
- Tests that are hard to read, without changing what they assert.

## Not yours

- **No new behaviour.** Not one branch that was not there. A bug you find is reported, not fixed.
- No mutation testing: that is `/dgf-harden`.
- No edits to the spec.
- No weakening a test to make a refactoring easier.

## 1. Measure before you touch anything

```bash
dgf changes --format agent              # every function the change touched: new, worse, better
dgf dup --changes --format agent        # copies with a copy in what the change touched
```

Then CRAP, which needs coverage: run the tests that cover the files the change touched, with a
coverage report (Cobertura, OpenCover or LCOV), and

```bash
dgf analyze <touched production files> --coverage <report or folder> --format agent
```

Read CRAP only for the functions `dgf changes` listed as new or changed. A scoped test run shows
every other function in those files as uncovered: that is the scope, not a finding.

## 2. Work from the top of the list

1. **The worst first, not the whole list.** A function with a cyclomatic score above 30 cannot
   get under CRAP 30 at any coverage: it has to be split.
2. **Split, then cover.** Four functions of 10 beat one of 40 with every test in the world.
3. Then names, duplication, boundaries.
4. Run the covering tests after each step, and measure again at the end. The number has to move.

How to bring a score down without gaming it is in the `dog-food` skill ("Reducing complexity").

## Duplication

Every group `dgf dup --changes` reports gets one of two outcomes, whether the change wrote the
copy or it was there before:

- **The copies mean the same thing and would change together: unite them.** Follow the group's
  `unite:` line and mind its `caution:`. Behaviour stays identical and every test stays green,
  also when that reaches into files the change did not touch.
- **They only look alike: leave them, with a one-line reason.** Copies that belong to features
  that will change separately, or that sit on two sides of a boundary that should stay a boundary.

A repeated query follows the same rule. A suggestion that changes every query at once (a global
filter) is new behaviour: carry it forward for the user, do not apply it.

Never record a decision in the duplication decisions file yourself: that file is the user's.

## Gate

- **No new or worse function above the limits:** `dgf changes` exits 0 and its `needs attention`
  section is empty. Old functions you only touched do not block, but they must not get worse.
- **CRAP at or under the limit** (30 unless `dgf.toml` says otherwise) for every function the
  change added or changed.
- **Never** a `// dgf-ignore`, `dgf baseline` or a raised limit to get there. If a function is
  honestly clearer as one piece (a parser, a state machine, generated code), say so and let the
  user decide.
- **Coverage did not drop** on any file you touched.
- **Every test is still green, and none changed what it asserts.**
- **Every duplication group is united or left with its reason.** Run `dgf dup --changes` again:
  it lists only the ones left.
- The build is clean for the projects your change affects.

Behaviour preservation is the hard rule. If a test had to change for a refactoring to pass, you
changed behaviour: revert that step and do it another way, and say in the report that it happened.

## Sign off

Commit your work on the current branch unless the project's instructions say otherwise. Do not
push or open a pull request unless asked. Then append to the `Log` of the work file and report the
same to the user:

```markdown
### clean · <date> · PASS | BLOCKED
Gate: dgf changes: <its first line>; CRAP <highest before> -> <highest after>;
  coverage of touched files held at <n>%; duplication: <n> united, <m> left
Changed: <files>
Duplication left: <file:lines> <-> <file:lines>, <reason>   (one line per group)
Carried forward: <what you could not fix and why; any bug you saw and left alone>
Next: /dgf-harden <name>
```

A 200-line function you could not split without changing behaviour is a legitimate carried
forward item, not a failure to hide.
