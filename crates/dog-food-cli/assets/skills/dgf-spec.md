---
name: dgf-spec
description: Stage 1 of 4 (spec, build, clean, harden). Turn a request, a user story or a ticket into a written spec that the later stages are checked against, by settling every open question before any code is written. Use when asked to spec, analyse or design a piece of work, or before /dgf-build when no spec exists.
argument-hint: "<a ticket, a user story, or a sentence that says what is wanted>"
---

# Spec: stage 1 of 4

Request: **$ARGUMENTS**

You turn a wish into a contract. You write **one file** and no code. The stages after you
(`/dgf-build`, `/dgf-clean`, `/dgf-harden`) start with no memory of this conversation: what is not
in the file does not exist for them.

That file is `.dgf/work/<name>.md`, where `<name>` is a short kebab-case name for the work
(`weekly-booking-limit`). If the project keeps executable specs (Gherkin features, acceptance
tests), the examples go there, in its format, and the file links to them.

## What you do not do

- No production code, no tests, no design. You say what must be true, not how to make it true.
- Read the code only to learn what the system does today. Never write class names, endpoints or
  table names into the spec: they are the builder's choice.
- Never soften a rule because it looks hard to build.
- Never a relative date. Not "today", not "next week": a literal `2026-08-21`.

## 1. Read before you ask

Most answers are already written down, and the person who can answer is the scarcest resource.
In this order:

1. The request itself, and **every comment on it**. A decision in a later comment overrides the
   description above it; say so in your report so the description can be corrected.
2. What it links to: the parent, related tickets, a plan or a design note, earlier decisions.
3. How the project already words the same situation, in its existing specs and tests.

Write down where each answer came from. An answer you can point to is a fact; one you made up is
the defect this stage exists to prevent.

## 2. Settle every question

You may not write the spec until each row is answered from the record, answered by the user, or
marked "does not apply" with a one-line reason.

| Must be settled | Why |
|---|---|
| For every limit: what happens one under, exactly at, and one over? | three examples, always |
| Who may do this, and who may not? What does each refused person see? | every refusal is its own example |
| Nothing there: no data, an expired one, zero results. What then? | empty states are skipped and then ship broken |
| Done twice: does the second attempt succeed, get refused, or do nothing? | |
| Which values are calendar days and which are moments in time? Whose time zone? | the classic off-by-one-day |
| Is it allowed in the past? On something cancelled, deleted or archived? | |
| What happens to data that already exists when this ships? | |
| What does the user see when something it depends on fails? | |

When the record is silent:

- **The user is here:** ask, several questions at once, each with your best guess as the first
  option so they can simply confirm. If an answer is vague, ask again with concrete options.
- **Nobody to ask:** decide the small things (the wording of a message, one of two equal
  phrasings) and list each under `Assumed`. For anything that changes the shape of the contract
  (who may, what a limit is, whether twice is legal, what happens to existing data), do not pick:
  write what you can, list the questions with your recommended answer for each, and stop.

Stopping with good questions is a pass for this stage. Guessing a limit and shipping it is the
failure.

## 3. Write the file

```markdown
# <Title: what is true when this is done>

Job: what is being built, in one or two sentences the person who asked could repeat.
Why: what is broken or missing today, and who feels it.
Guardrails: what must stay true while this is built, one by one. Each is a sentence a later
  stage can be failed against.
Done means: how anyone can check it is finished. Never "works well".
Out of scope: what this deliberately does not do, so nobody builds it by accident.

## Rules

- A member at their weekly limit cannot book again.
  Given "Kate" has 4 bookings in the week of 2026-08-16 on a plan of 4 a week,
  when she books a class on 2026-08-21, then the booking is refused and she is told why.
- ...

## Assumed
- <each decision you made that the user did not>

## Open questions
- <each question that stops the work, with your recommended answer>

## Log
```

- **The language of the business.** No URLs, status codes or screen furniture ("clicks the
  button"). If a sentence could not be said aloud to the person who asked, rewrite it.
- **One rule, one sentence a person can tick off**, then one example of it with real names and
  literal values. A limit gets three examples.
- **One action per example.** Two "when"s are two examples.
- Every answer from step 2 shows up as at least one rule.

If the project has executable specs, write the examples there as well and run them: they must
**fail because the behaviour is missing**, not because of a typo or a compile error. An example
that already passes describes something that exists: take it out or find out why.

## Gate

- Every row of step 2 is settled or marked "does not apply".
- Every rule has an example, with literal values.
- `Open questions` is empty, or the stage ends as blocked with the questions as its result.

## Sign off

Append to the `Log` of the work file, then report the same to the user:

```markdown
### spec · <date> · PASS | BLOCKED
Gate: <n> rules, <m> examples; <executable examples: n failing for the right reason | none>
Assumed: <n>, listed above
Carried forward: <what the builder must not rediscover: a decision, a trap, a source worth reading>
Next: /dgf-build <name>
```

If you know which folder the work will change, add one line to `Carried forward` from
`dgf analyze <folder> --format agent`: its verdict line tells the builder what it is walking into.
