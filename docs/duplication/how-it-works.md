# How the syntactic engine works

[Duplication](README.md) › How the syntactic engine works

Every step is exact on the syntax tree (tree-sitter), with no AI and no guessing. It runs on all cores and scans 2,000 files in about 2 seconds.

## 1. Statements become shapes

Each statement becomes its shape: keywords, operators and structure, with names and literals replaced by placeholders. Renamed copies have the same shapes.

## 2. Exact and renamed copies (types 1 and 2)

- From each statement, the shortest run with at least `--min-tokens` tokens (50) and `--min-lines` lines (5) is a **seed**.
- Seeds with the same shapes form a group. The group grows one statement at a time and splits where its members' next statements differ. No pairs are compared, so a pattern copied hundreds of times costs no more than its copies.
- **Anti-unification** of the copies' tokens gives the template: a hole wherever names or values differ, one hole per distinct set of differences.

## 3. Near misses (type 3)

- Whole function bodies of at least 25 tokens are compared. Constructors are left out: they are mostly wiring.
- Bodies sharing enough runs of 8 normalized tokens are candidates.
- A candidate pair is aligned in two steps: statements by shape (a shortest edit script, Myers' algorithm), then tokens only inside statements that differ and are at least 70% alike.
- A pair matching at least 80% of both bodies' tokens is a near miss. Connected pairs form a group around the body most like the others.

## 4. Reordered and mixed-in copies

- Statements of two blocks are paired by shape, renaming locals consistently. Members and called functions keep their names.
- A pair is kept only when the data flows the same way: each name it reads comes, in both copies, from partnered statements or from outside the copy.
- The largest set of paired statements linked by what they define and read is the copy. Statements that only look alike, such as a list of independent calls, are not one.
- The report lists the copy's lines and the statements in between, which need moving out of the way before extracting it.

## 5. Long expressions

A long expression inside a statement is a block of its own: a call chain such as a LINQ query, an object built with its members, a `switch` expression, three lines or more. So a copy of it is found where the statements around it differ: `var rows = query...;` in one method, `foreach (var row in query...)` in another.

## 6. What is left out

- a copy inside a bigger copy, or overlapping another copy of its group;
- a group that only wraps a group with more copies, or lies inside another group's copies of the same functions;
- code that shares a shape but little else: more than two parameters per statement (mapping code, guard clauses on other fields);
- statements that only configure what the function was given: calls on a parameter that assign nothing, such as EF Core's `builder.Property(...)` rows, `services.AddScoped<A, B>()` and `options.AddPolicy(...)`. These are registration tables, not copies to unite;
- an exact copy that is most (70%) of a near-miss pair's bodies wins over the near miss, as extracting it is the cleaner refactor;
- test code (unless `--tests`), migrations and generated files.

The code is in [crates/dog-food-dup/src](../../crates/dog-food-dup/src): `find.rs` (seeds, groups, templates), `near.rs` (near misses), `reorder.rs` (reordered copies), `tokens.rs` (shapes and blocks).
