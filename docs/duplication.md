# Duplication: `dgf-dup`

`dgf-dup` finds code that is the same apart from names and values, or that is a near copy (a statement added, left out or changed), and says how to unite it. It is its own tool (`crates/dog-food-dup`) and a library, so `dgf` can put duplication next to complexity, coverage and mutation results later.

```powershell
cargo install --path crates/dog-food-dup --locked
dgf-dup                        # the current folder, honoring .gitignore
dgf-dup backend --top 10       # the ten biggest wins
dgf-dup src --format agent     # compact text for AI assistants
dgf-dup src --format json      # everything, for other tools
dgf-dup src --format html -o dup.html   # a page: the files with their copies, and each group
dgf-dup src --fail-above 5     # exit 1 when more than 5% of lines are copies

dgf-dup --changes              # only what this branch changed (since main, else master)
dgf-dup --changes --base origin/development --fail-on-copies   # a PR gate: exit 1 on any new copy
```

With `--changes`, the whole repository is still scanned, since a new copy is usually a copy of code the branch did not touch, but only groups with a copy in the branch's changed lines are reported (uncommitted edits and new files count). Changed copies are marked `(changed)`.

The HTML page draws every file with copies to scale, with a mark per copy in its group's color; clicking a file or a mark shows the groups it is in. Each group is a card with its type, its copies, the template with each hole highlighted (and, for near copies, what not every copy has), a table of what each copy has in each hole, where near copies part ways, and how to unite them. It filters by file, function or code, and by "only changed".

C#, TypeScript/JavaScript (also TSX/JSX) and Rust. Test code (`*.spec.ts`, `*Tests.cs`, `tests/`, `e2e/`, Rust `mod tests`), generated files and migrations are left out unless `--tests` is given.

## What a finding looks like

```
1. 2 copies of 10 lines (3 statements), type 2: renamed, saves about 6 lines
   src/Billing.cs:4-13   in Orders.Add
   src/Billing.cs:20-29  in Seats.Add
   template:
     | var total = 0m;
     | if (⟨1⟩.Active && ⟨1⟩.End > now)
     | {
     |     total += ⟨1⟩.⟨2⟩ * ⟨1⟩.Quantity;
     |     log.Add($"{⟨1⟩.Id} counted");
     | }
     | if (total > limit)
     | {
     |     total = limit;
     | }
   ⟨1⟩ name: plan | seat
   ⟨2⟩ member: Price | Fee
   ⟨3⟩ local: total | sum
   unite: extract a shared function in this file (a static helper the types call), taking ⟨2⟩ members; ⟨1⟩ variables (...)
   caution: the code after the first copy uses total: a shared version returns it

2. 2 copies of 8 lines (4 statements), type 3: near miss, 91% alike, saves about 3 lines
   src/Invoices.cs:40-47  in Invoices.Lines
   src/Receipts.cs:99-110  in Receipts.Lines
   template:
     | var rows = new List<string>();
     | foreach (var line in order.Lines)
     | {
     |     var amount = line.Quantity * line.Price;
     |     rows.Add(line.Sku + " " + line.Quantity + " " + amount);
     | }
     | rows.Sort(StringComparer.Ordinal);
     | return rows;
   differs: Receipts.Lines (line 102) adds `if (line.Quantity == 0) { continue; }`
   unite: the functions' bodies are alike apart from 1 edit: keep one function and pass what differs as a parameter, a flag or a delegate (...)

3. 8 statements in a row with one shape, saves about 11 lines
   src/Users.cs:55-77  in UserService.Update
   template:
     | if (dto.⟨1⟩ != null)
     |     user.⟨1⟩ = dto.⟨1⟩;
   ⟨1⟩ member: City | State | PostalCode | Country | ...
   unite: 8 statements in a row share this shape (23 lines): put what differs (⟨1⟩ members) in a list and loop over it, or call one helper 8 times
```

- **Type**: 1, the same code; 2, renamed (names, values or types differ); 3, a near miss: whole function bodies whose tokens match for at least 80% of both, with statements or tokens added, left out or changed.
- **Copies**: where each one is and the function it is in.
- **Template**: the first copy, with `⟨n⟩` wherever the copies differ. The same differences everywhere are one hole.
- **Holes** and what a shared version does with them: a *value* or *name* becomes a parameter, a *member* becomes a value or a selector (`x => x.Price`), a *call* becomes a function parameter, a *type* a generic parameter. A *local* the copy declares itself is only renamed and needs no parameter.
- **Unite**: keep one function (when whole bodies are copies), extract a private method (same type), a shared function (same file) or a shared helper (several files). A run of statements with one shape in a row (`if (dto.X != null) user.X = dto.X;` for every field) is reported once, with the advice to loop over a table of what differs.
- **Differs** (near misses): what each copy adds, leaves out or has instead, against the first copy, with its line. In the template, `⟪…⟫` marks what not every copy has.
- **Cautions**: values the code after a copy uses (the helper returns them), and `return`/`break`/`continue` inside a copy.
- **Saves**: the lines a shared version would remove, after its signature and calls. Findings are ranked by it.

## How it works

No AI, and no guessing: every step is exact on the syntax tree.

1. Each statement becomes its shape: keywords, operators and structure, with names and literals replaced by placeholders. Renamed copies have the same shapes.
2. The shortest run of statements from each position with at least `--min-tokens` tokens and `--min-lines` lines is a seed. Seeds with the same shapes form a group, and the group grows one statement at a time, splitting where its members' next statements differ. Every copy grows as far as another stays alike, with no pairwise comparison, so a pattern copied hundreds of times costs no more than its copies.
3. Equal runs form a clone class. A copy inside a bigger copy, a copy that overlaps another copy of its class, and a class that only wraps a class with more copies are left out. So is a class with more than two parameters per statement: code that shares a shape but little else (mapping code, guard clauses on other fields).
4. Anti-unification of the copies' tokens gives the template: a hole at each position where their names or values differ, one hole per distinct set of differences.
5. Near misses (type 3) compare whole function bodies of at least 25 tokens (constructors, which are mostly wiring, are left out). Bodies that share enough runs of 8 normalized tokens are candidates. A candidate pair is aligned in two steps: statements by shape (a shortest edit script, Myers' algorithm), then tokens only between statements that differ, and only where two statements are at least 70% alike, so unrelated statements in the same place do not lend each other tokens. Pairs matching at least 80% of both bodies' tokens are near misses. Connected pairs form a class around the body most like the others. When an exact copy is most (70%) of each body, the exact copy is reported instead, as extracting it is the cleaner refactor.

The same logic written differently (a loop and its LINQ, `if` and `?:`) is not found yet: that needs the compiler's view of the code. See [the research and plan](research/csharp-semantic-duplication.md).

## Measuring it

[crates/dog-food-dup/tests/corpus/csharp](../crates/dog-food-dup/tests/corpus/csharp/README.md) is a labelled C# corpus: copies (types 1-3), groups of methods that do the same thing written differently (type 4), near misses that do different things, and decoys. Running it checks its own labels on generated inputs. `cargo run -p dog-food-dup --bin dgf-dup-bench` scores `dgf-dup` on it, or any tool's JSON with `--report`. Now: types 1-3 14 of 14, no decoy matched, no unrelated match; type 4, 1 of 47.
