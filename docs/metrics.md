# Metrics

Every function, method, constructor, accessor, local function, lambda, closure and arrow function gets its own scores. A lambda inside a method is a separate entry (`OrderService.Run/abs`) and does not add to the method.

## Cyclomatic complexity (`complexity`)

The number of independent paths through a function (McCabe, 1976): **1 plus every decision point**. It is also the minimum number of test cases needed to run every path once. All limits (`warn`, `fail`, overrides, baseline) use this number.

| Construct | C# | TypeScript / JavaScript | Rust |
| --- | --- | --- | --- |
| `if` / `else if` | +1 each | +1 each | +1 each |
| Loops | `for`, `foreach`, `while`, `do` | `for`, `for…of`, `for…in`, `while`, `do` | `for`, `while` (not `loop`) |
| `switch` / `match` | each `case` label (not `default`); switch expression: arms − 1 | each `case` (not `default`) | `match`: arms − 1 |
| `catch` | +1 | +1 | – |
| `?:` | +1 | +1 | – |
| `&&`, `\|\|` | +1 each | +1 each | +1 each |
| `??` | +1 | +1 | – |
| Pattern `and` / `or` | +1 each | – | – |
| Not counted | `?.`, `return`, `break`, `goto` | `?.` | `?`, `loop` |

The `breakdown` in every report lists what was counted, for example `if ×2, case ×3, && ×1`.

## Cognitive complexity (`cognitive`)

How hard the function is to *read*, following SonarSource's definition. Nesting is what makes code hard to follow, so:

- every branch or loop (`if`, loops, `switch`/`match`, `catch`, `?:`) adds **1 plus its nesting depth**;
- every `else` and `else if` adds 1, with no nesting penalty;
- every run of the same logical operator adds 1: `a && b && c` is +1, `a && b || c` is +2.

```typescript
function primes(max: number) {
  for (let i = 1; i < max; i++) {        // +1
    for (let j = 2; j < i; j++) {        // +2 (nesting 1)
      if (i % j === 0) { }               // +3 (nesting 2)
    }
  }
}                                        // cyclomatic 4, cognitive 6
```

A flat `switch` with ten cases is cyclomatic 11 but cognitive 1: many paths, easy to read. Three nested loops are the other way round. Looking at both tells you whether a function needs more tests (cyclomatic) or a restructure (cognitive).

Differences from SonarSource: lambdas are scored separately instead of adding nesting to their parent; labelled jumps and recursion are not counted.

## Shape

- **`maxNesting`**: deepest nesting of branches and loops.
- **`lines`**: from the function's first line to its last, including blank lines and comments.
- **`parameters`**: declared parameters (`self` in Rust counts).

## Coverage and CRAP

With `--coverage` (or `[coverage] paths`), every function the report covers also gets:

- **`coverage`**: covered share of its instrumented lines, 0 to 1;
- **`crap`**: Change Risk Anti-Patterns (Savoia and Evans, 2007): `complexity² × (1 − coverage)³ + complexity`.

Fully covered, CRAP equals the complexity. Uncovered, it grows with the square of the complexity: complexity 10 at 0% is 110. Above 30 is the classic "crappy" line (`crap_warn`, default 30). A CRAP score above `crap_warn` / `crap_fail` raises the function's status like the complexity limits do. See [coverage.md](coverage.md).

## Status

| Status | Meaning |
| --- | --- |
| `ok` | Within the limits |
| `warn` | Above `warn` (or `crap_warn`); the run still passes |
| `fail` | Above `fail` (or `crap_fail`); exit code 1 |
| `ignored` | Excused by a `// dgf-ignore: reason` comment or the baseline; listed, never fails |

## Known limits

- Syntax only: no type information. Code generated at build time is not seen.
- Code inside Rust macro calls (`matches!(…)`, `println!(…)`) is not analyzed.
- Decisions outside any function (top-level script code) are not counted.
