# Duplication: `dgf-dup`

`dgf-dup` finds code that is the same apart from names and values, and says how to unite it. It is its own tool (`crates/dog-food-dup`) and a library, so `dgf` can put duplication next to complexity, coverage and mutation results later.

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

The HTML page draws every file with copies to scale, with a mark per copy in its group's color; clicking a file or a mark shows the groups it is in. Each group is a card with its copies, the template with each hole highlighted, a table of what each copy has in each hole, and how to unite them. It filters by file, function or code, and by "only changed".

C#, TypeScript/JavaScript (also TSX/JSX) and Rust. Test code (`*.spec.ts`, `*Tests.cs`, `tests/`, `e2e/`, Rust `mod tests`), generated files and migrations are left out unless `--tests` is given.

## What a finding looks like

```
1. 2 copies of 10 lines (3 statements), saves about 6 lines
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

2. 8 statements in a row with one shape, saves about 11 lines
   src/Users.cs:55-77  in UserService.Update
   template:
     | if (dto.⟨1⟩ != null)
     |     user.⟨1⟩ = dto.⟨1⟩;
   ⟨1⟩ member: City | State | PostalCode | Country | ...
   unite: 8 statements in a row share this shape (23 lines): put what differs (⟨1⟩ members) in a list and loop over it, or call one helper 8 times
```

- **Copies**: where each one is and the function it is in.
- **Template**: the first copy, with `⟨n⟩` wherever the copies differ. The same differences everywhere are one hole.
- **Holes** and what a shared version does with them: a *value* or *name* becomes a parameter, a *member* becomes a value or a selector (`x => x.Price`), a *call* becomes a function parameter, a *type* a generic parameter. A *local* the copy declares itself is only renamed and needs no parameter.
- **Unite**: keep one function (when whole bodies are copies), extract a private method (same type), a shared function (same file) or a shared helper (several files). A run of statements with one shape in a row (`if (dto.X != null) user.X = dto.X;` for every field) is reported once, with the advice to loop over a table of what differs.
- **Cautions**: values the code after a copy uses (the helper returns them), and `return`/`break`/`continue` inside a copy.
- **Saves**: the lines a shared version would remove, after its signature and calls. Findings are ranked by it.

## How it works

No AI, and no guessing: every step is exact on the syntax tree.

1. Each statement becomes its shape: keywords, operators and structure, with names and literals replaced by placeholders. Renamed copies have the same shapes.
2. The shortest run of statements from each position with at least `--min-tokens` tokens and `--min-lines` lines is a seed. Seeds with the same shapes are compared in pairs, and each pair grows as far as both stay alike.
3. Equal runs form a clone class. A copy inside a bigger copy, a copy that overlaps another copy of its class, and a class that only wraps a class with more copies are left out.
4. Anti-unification of the copies' tokens gives the template: a hole at each position where their names or values differ, one hole per distinct set of differences.

Statements that only resemble each other (an extra or a changed statement in the middle) and the same logic written differently are not found yet; see the roadmap.
