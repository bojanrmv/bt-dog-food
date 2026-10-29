# Mutators

[Mutation testing](README.md) › Mutators

`--level` (or `level`) picks `basic`, `standard` (the default) or `advanced`. In TypeScript and JavaScript, `basic` keeps only the operators and block removal. Names in `ignore_mutators` and in `dgf-disable` comments are the ones in the left column, in any case.

| Mutator | Level | C# | TypeScript |
| --- | --- | --- | --- |
| `arithmetic` | basic | `+` ↔ `-`, `*` ↔ `/`, `%` → `*`; not on strings | the same |
| `relational` | basic | `<` → `>`, `<=`; `<=` → `>`, `<`; `>` → `<`, `>=`; `>=` → `<`, `>`; also in patterns (`is > 5`) | `<` → `<=`, `>=`; `<=` → `<`, `>`; `>` → `>=`, `<=`; `>=` → `>`, `<` |
| `equality` | basic | `==` ↔ `!=`; `x is P` ↔ `x is not P` | `==` ↔ `!=`, `===` ↔ `!==` |
| `logical` | basic | `&&` ↔ `\|\|`; pattern `and` ↔ `or`; `a ^ b` → `a == b` | `&&` ↔ `\|\|`, `??` → `&&` |
| `bitwise` | basic | `&` ↔ `\|`, `<<` → `>>`, `>>>` and back; `a ^ b` → `~(a ^ b)` | – |
| `block` | basic | a block → `{}`, or `{return default;}` where the method returns a value; only when no other mutant is inside it | a block → `{}` |
| `nullcoalescing` | basic | `a ?? b` → `b`, `a`, `b ?? a` | – |
| `statement` | standard | `Save();`, `return;`, `break;`, `continue;`, `goto`, `throw`, `yield` → `;` | – |
| `call` | standard | – | `save();` and `throw x;` → `;`, when nothing else inside is mutated |
| `boolean` | standard | `true` ↔ `false` | the same |
| `negation` | standard | `!x` → `x` | the same |
| `negate` | standard | `if (c)`, `while (c)` → `!(c)`, unless `c` is `!x`, `==`, `!=` or `is` | – |
| `condition` | standard | an `if` condition → `false` | an `if` or `?:` test → `true` and `false`; a loop test → `false`; a boolean expression → `true` and `false`; a `case` loses its statements |
| `conditional` | standard | `c ? a : b` → `true ? a : b`, `false ? a : b` | – |
| `unary` | standard | `-x` ↔ `+x`, `~x` → `x` | the same |
| `update` | standard | `x++` ↔ `x--`, `++x` ↔ `--x` | the same |
| `assignment` | standard | `+=` ↔ `-=`, `*=` ↔ `/=`, `%=` → `*=`, `&=`, `\|=`, `^=`, shifts, `??=` → `=` | `+=` ↔ `-=`, `&&=` ↔ `\|\|=`, `??=` → `&&=` and more |
| `checked` | standard | `checked(e)` → `e` | – |
| `linq` | standard | `First` ↔ `FirstOrDefault`, `Any` ↔ `All`, `OrderBy` ↔ `OrderByDescending`, `Skip` ↔ `Take`, `Sum` → `Max`, `Count` → `Sum` and more pairs; not to a method that needs an argument the call lacks | – |
| `string` | standard | `"a"` → `""`, `""` → `"dgf was here!"`, `$"..."` → `$""`, `string.Empty`, `string.IsNullOrEmpty(x)` → `x != null`, `x != ""` | `"a"` → `""`, `` `a` `` → ` `` ` `` |
| `initializer` | standard | `new T { A = 1 }`, `new List<int> { 1 }`, `new int[] { 1 }`, `int[] a = { 1 }` → `{ }` | – |
| `array`, `object` | standard | – | `[a, b]` → `[]`, `[]` → `["dgf was here"]`, `new Array(3)` → `new Array()`; `{ a: 1 }` → `{}` |
| `arrow`, `optional`, `method` | standard | – | `() => a` → `() => undefined`; `a?.b` → `a.b`; `s.trim()` → `s`, `startsWith` ↔ `endsWith` and more |
| `math`, `stringmethod`, `collection` | advanced | `Math.Floor` ↔ `Math.Ceiling` and the other `Math` swaps; `StartsWith` ↔ `EndsWith`, `ToUpper` ↔ `ToLower`, `s.Trim()` → `""`; `[a, b]` → `[]` | – |
| `regex` | advanced (TypeScript: standard) | the pattern of `new Regex("...")`, and of `Regex.IsMatch(s, "...")` and the other static methods too: sixteen kinds of change, such as `^` and `$` removed, `[a-z]` → `[^a-z]`, `[b-z]`, `[\w\W]`, `\d` → `\D`, `d`, `[\d\D]`, `a+` → `a`, `a{0,}`, `a{2,}`, `a+?`, `(a)` → `(?:a)`, `(?=a)` → `(?!a)` | a regex literal or `new RegExp("...")`: the six that suit JavaScript: `^` and `$` removed, class and `\d` negation, quantifier removal, lookaround negation |

## Mutants that are not made

Each failed `dotnet build` costs a full rebuild, so dgf decides before building which mutants could not compile:

- **Not made, because they cannot compile:** `+` and `+=` where one side is a `string` variable of the same function; `x is T v` → `x is not T v` and `!(c)` when the declared `v` is used in the branch; `&&` ↔ `||` when the left side declares a variable the right side uses; removing the last `throw` of a function that returns a value; removing a block in a method with `out` parameters; a `?.` chain is never cut in the middle (that would also change what the unmutated code does).
- **Not made, because they cannot change behaviour:** removing a `return;` or `continue;` that is the last statement of its function or loop.
- **Regex patterns** are parsed by dgf; a pattern it does not understand gets no regex mutants, and a mutated pattern that no longer parses is dropped, since `new Regex` would reject it (a group made non-capturing while `\1` refers to it). An interpolated pattern gets none.
- **Copies that keep their lines:** a mutant whose switch has to copy a string spanning lines or an `#if` line keeps the copy's line breaks between `#line` directives, so every line keeps its number for the compiler. In TypeScript, a line break in a template string is written `\n` in the copy; a tagged template (`` String.raw`...` ``) spanning lines cannot be copied, and a mutant that would copy one is not made.
- **`#if` with branches that only parse one at a time** (an expression body in each branch): only the first branch is mutated.
- **Also made:** `condition` for C# (an `if` condition → `false`), and regex mutants for `Regex`'s static methods.
- In TypeScript, the sandbox adds `// @ts-nocheck` to mutated files, so a mutant only has to be valid syntax.

## Expressions that declare a variable

(`o is Order order`, `out var n`) are mutated too. A switch normally copies the mutated expression next to the original, which would declare the variable twice; such a mutant switches the whole statement instead (`{if (switch) {mutated statement} else {original statement}}`), or stays inside the expression when its mutated copy no longer declares the variable (a condition → `false`). When the variable is used after the statement, neither works, and the mutant is not made. A mutant that only changes a condition's value (`condition`, `conditional`, `negate`, `negation`) is kept even then, when every variable the condition declares is an `out` argument that is always evaluated: it becomes `(c) && !switch`, `(c) || switch` or `(c) != switch`, so `!map.TryGetValue(k, out var v)` still runs once and `v` is assigned as before. The condition is still evaluated, so a side effect in it (the `TryGetValue` itself) still happens.

## Code that is hard to mutate

Some code has mutants that cannot compile. dgf keeps the function and leaves out only those mutants, mostly before building:

- `out var` in conditions (`if (!int.TryParse(s, out var n)) return;`), as above;
- pattern variables used after the `if` (`if (o is not string t) return 0; return t.Length;`): turning that condition to `false` leaves `t` unassigned, which no tool can compile, so those mutants are not made and the rest of the function is mutated;
- a local assigned in `try` whose `catch` returns or throws, and an `if` that returns when a `TryGet` fails: the block or jump whose removal would leave the variable unassigned is not mutated, the rest of the function is;
- `out` parameters assigned inside `try`/`catch`;
- `while (true)` with a `return` inside, in a function that returns a value: the loop condition is not mutated (`false` would make the end of the function reachable without a value);
- `??` whose left side is a nullable value (`decimal? price ?? 0`): the right side alone is kept, the left side alone is not made (it would be `decimal?` where a `decimal` is needed).

When a mutant slips through and the compiler reports "use of unassigned local variable" (CS0165) or "not all code paths return a value" (CS0161), the error points at a use or at the end of the function, not at the mutant: every mutant of that function becomes a suspect and is put back half at a time until the culprit is found.

## New C#

The C# grammar (tree-sitter-c-sharp 0.23) predates C# 14 and 15. Before mutating, dgf rewrites what it does not know into code of the same length that it does, so every byte keeps its place: `closed` classes, labeled `break` and `continue`, null-conditional assignment (`a?.B = c`), `extension` blocks and file-based app directives (`#:package`). Only the parse sees the rewrite; the mutants are made and compiled on the original text. `Modern.cs` in the fixtures uses C# 12 to 15 and is compiled with every mutant switched in when a .NET 11 SDK is installed.

## For contributors

Each mutant is one byte range and its replacement (`dog_food_core::mutate::mutate`), so it can be applied alone (`Mutant::apply`) or compiled in next to the others (`dog_food_core::mutate::schemata`). `crates/dog-food-core/tests/mutants/` holds a C# and a TypeScript file that state, line by line, which mutants each line must get; the C# one is also compiled with every mutant switched in.

To leave mutants out on purpose: [turning mutants off](turning-off.md).
