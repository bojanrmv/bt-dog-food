# Turning mutants off

[Mutation testing](README.md) › Turning mutants off

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

## Date boundaries

A comparison of dates or times (`today <= plan.LastDay`, `x is > DateOnly.MinValue`) is marked **boundary-risk** in every report: a surviving `<=` → `<` there is the off-by-one-day bug. Without types, dgf calls an operand a date when the file declares it as `DateOnly`, `DateTime`, `DateTimeOffset` or `TimeOnly` (a parameter, local, field or property, or a `var` set from one), when its name says so (`EndDate`, `LastDay`, `CreatedAt`, `ExpiresOn`, `today`, `UtcNow`), or when it builds one (`DateOnly.FromDateTime(...)`). Such a mutant may only be turned off by a `dgf-disable` comment whose reason names the test that pins the boundary, `// dgf-disable-once relational: the last day counts, test: PlanTests.EndsOnLastDay`; dgf checks that the test exists in the test code and stops with exit code 2 otherwise.
