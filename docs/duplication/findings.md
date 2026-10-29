# Reading a finding

[Duplication](README.md) › Reading a finding

Each finding is a group of copies, ranked by the lines a shared version would save (or by risk, with `--coverage`).

```
1. 2 copies of 10 lines (3 statements), type 2: renamed, saves about 6 lines
   src/Billing.cs:4-13   in Orders.Add
   src/Billing.cs:20-29  in Seats.Add
   template:
     | var total = 0m;
     | if (⟨1⟩.Active && ⟨1⟩.End > now)
     | {
     |     total += ⟨1⟩.⟨2⟩ * ⟨1⟩.Quantity;
     | }
   ⟨1⟩ name: plan | seat
   ⟨2⟩ member: Price | Fee
   risk: CRAP 42 in Orders.Add (complexity 6, 0% covered)
   unite: extract a shared function in this file, taking ⟨2⟩ members; ⟨1⟩ variables
   caution: the code after the first copy uses total: a shared version returns it
```

## The parts

| Part | Meaning |
| --- | --- |
| **Type** | 1 the same code · 2 renamed · 3 a near miss · 4 the same behaviour written differently (C#). A type 3 marked *same statements and data flow* is a reordered or mixed-in copy. |
| **Copies** | file, lines and function of each copy. `(changed)` with `--changes`. `(lines 5-6, 8-11)` for a mixed-in copy: only those lines are part of it. |
| **Template** | the first copy, with `⟨n⟩` where the copies differ. `⟪…⟫` marks what not every copy has (near misses). Type 4 shows the canonical expression every copy computes. |
| **Holes** | what differs, and what it becomes in a shared version: a *value* or *name* a parameter, a *member* a selector (`x => x.Price`), a *call* a function parameter, a *type* a generic parameter. A *local* is only renamed. |
| **Differs** | near misses: what each copy adds, leaves out or has instead, with its line. |
| **Same after** | type 4: the rules that made the methods equal (`loop that adds up → Sum`), and any condition they need ("equal when the total does not overflow int"). |
| **Confirmed** | type 4 with `--confirm`: the methods returned the same results on 1,000 generated inputs, or why they were not run. |
| **Risk** | the riskiest copy's function: its cyclomatic complexity, and with `--coverage` its covered share and CRAP score. |
| **As a method** | C# with `--semantic`: what the first copy takes and returns if extracted, from the compiler's data flow, and whether it needs `this`. |
| **Unite** | what to do: keep one function, extract a private method, a shared function in the file, or a helper the files share. |
| **Caution** | what makes uniting harder: values used after the copy (the helper returns them), `return`/`break`/`continue` inside it, statements between the parts of a mixed-in copy. |
| **Saves** | lines removed by a shared version, after its signature and calls. |

## Formats

| Format | For |
| --- | --- |
| `table` (default) | reading in the terminal; `--top N` groups |
| `agent` | AI assistants: every group, compact, with the template and advice |
| `json` | other tools: everything, including each copy's code |
| `html` | people: the [review dashboard](review.md) |
