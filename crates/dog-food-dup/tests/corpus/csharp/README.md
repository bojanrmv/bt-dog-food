# C# duplication corpus

Labelled C# code for measuring duplication finders, `dgf-dup` and the engines that come after it. The code is a small order and invoice domain ([Model.cs](Model.cs)), and every method in [Cases/](Cases/) is part of a labelled case in [cases.json](cases.json):

| File | What it holds | Label |
| --- | --- | --- |
| [Cases/Copies.cs](Cases/Copies.cs) | Code pasted and edited: the same code, renamed, a statement added or changed | pairs, Types 1-3 |
| [Cases/Equivalent.cs](Cases/Equivalent.cs) | Groups of methods that do the same thing written differently: `for`/`foreach`/`while`/LINQ, `if`/`?:`/`switch`, early return/nested `if`, locals/inlined, reordered statements, string building, null checks, De Morgan | groups, Type 4 |
| [Cases/NearMisses.cs](Cases/NearMisses.cs) | Pairs one edit apart that do different things: `>=`/`>`, `First`/`FirstOrDefault`, `Any`/`All`, a dropped null check, swapped arguments, integer division, off by one | pairs, not equal |
| [Cases/Decoys.cs](Cases/Decoys.cs) | Same shape, unrelated purpose | pairs, Type 0 |

## The labels are checked

`dotnet run` runs every labelled pair on generated inputs ([Check/Program.cs](Check/Program.cs)):

- pairs labelled equal, and all members of a group, must agree on 500 inputs;
- pairs labelled not equal must differ on at least one input, which it prints.

Inputs come from the parameter types: records through their constructors, lists of 0-4 items, nulls only where the type is nullable, and the edge values code branches on (0, 100, empty strings). A thrown exception counts as a result, so `First` throwing and `FirstOrDefault` returning 0 differ.

## Scoring

```
cargo run -p dog-food-dup --bin dgf-dup-bench                       # dgf-dup on the corpus
cargo run -p dog-food-dup --bin dgf-dup-bench -- --report other.json # any tool's JSON in dgf-dup's format
```

A report matches a pair when one clone class has copies in both methods. A class claims equal behaviour when its `evidence` is `normalized`, `tested` or `proven`. The scorecard gives:

- recall per clone type, and for Type 4 per group;
- precision: of the matched labelled pairs, the share that are clones (a match of a Type 0 pair is wrong);
- equal claims, right and wrong: calling a near miss equal is the worst error;
- matched pairs the labels do not cover. Many cases share a "sum the lines" loop on purpose, as real code does, so these are listed for review, not counted as wrong.

`cargo test -p dog-food-dup --test corpus` holds dgf-dup at the scores reached so far and runs the label check.

## Adding a case

Add the methods to a file in `Cases/` (static, in a static class, with parameter types the checker can generate), label them in `cases.json`, and run `dotnet run` until every label holds.
