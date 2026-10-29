# dgf-dup-cs: C# that does the same thing, written differently

`dgf-dup` finds copies: code with the same shape. This engine finds C# methods that compute the same result even when the code looks different. Examples are a `for` loop and its LINQ, early returns and nested `if`s, `?:` and a `switch` expression, and `x == null` and `x is null`. It reduces every method to one canonical expression with the compiler (Roslyn), and methods with equal expressions are reported together as type 4, with evidence `normalized`.

```powershell
# install (once): a .NET tool
dotnet pack crates/dog-food-dup/semantic -c Release
dotnet tool install -g dgf-dup-cs --add-source crates/dog-food-dup/semantic/nupkg

dgf-dup --semantic                       # both engines, one report (any format, --changes too)
dgf-dup-cs src -o semantic.json          # the engine alone, dgf-dup's JSON
dgf-dup-cs src --explain -o NUL          # every method's canonical form, or why it was left out
```

`dgf-dup --semantic` runs `dgf-dup-cs` from PATH, `DGF_DUP_CS`, or `--semantic-engine` (an executable or its `.dll`). A class the syntactic engine already reports for the same functions is not repeated.

## How it works

1. **One compilation.** All `.cs` files under the paths are compiled together. The references are the running .NET's assemblies, the packages of restored projects (from their `obj/project.assets.json`) and ASP.NET Core's shared framework when a project uses it. The SDK's implicit usings are added too. No build is needed, only a restore. Generated files, migrations, `bin`/`obj` and (unless `--tests`) tests are left out.
2. **Each method runs symbolically** ([Exec.cs](Exec.cs)), over Roslyn's operation tree, to the value it returns:
   - locals hold expressions, so they disappear (inlined);
   - an `if` splits the run: each branch goes on with the rest of the body and knows the test's outcome, so early returns, nested `if`s, `?:` and `switch` end as the same decision tree;
   - a local holding a value with effects (an `await`, a call to unknown code, a new object) is bound once (`let`) instead of copied to its uses, so one call stays one call.
3. **Expressions are built in one normal form** ([Canon.cs](Canon.cs)):
   - booleans are decision trees over atomic tests, keeping C#'s evaluation order;
   - comparisons have one polarity (`a > b` is `b < a`; `a >= b` is `!(a < b)` only for total orders: integers, decimals, enums, dates; never `double`);
   - a condition inside an expression is lifted out (`f(c ? a : b)` is `c ? f(a) : f(b)`);
   - commutative arithmetic sorts its operands, constants fold (`0` as a decimal is `0m`), `x + 0` is `x`;
   - strings built with `+`, interpolation or a `StringBuilder` are one concatenation;
   - `a ?? b`, `a?.B`, `string.IsNullOrEmpty`, `Math.Max` and `Min`, and patterns (`is null`, `is > 0`, `is { A: 1 }`, `and`, `or`, `not`) become the tests they make.
4. **Loops are what they compute** ([Loops.cs](Loops.cs), [Folds.cs](Folds.cs)). An index `for`, a `while` over `i < xs.Count` and a `foreach` all walk the elements. The body runs once with each changed local as a placeholder, and how the body ends says which loop it is:
   - **fold**, every element changes the locals the same way under a guard: `Sum`, `Count`, `Max` (only where the path proved the list is not empty), `Where`/`Select`/`ToList`, and a sum per key (`GroupBy(...).ToDictionary(g => g.Key, g => g.Sum(...))`);
   - **search**, some ends `return`: `Any(c) ? v(First(c)) : rest`;
   - **flag**, some ends set locals and `break`: the first match decides their values.

   LINQ calls ([Calls.cs](Calls.cs)) map to the same vocabulary, with `All(c)` as `!Any(!c)`, `Where` then `Where` as one `Where`, `Count == 0` as "not any", and `FirstOrDefault(c) == null` as "not any" (when elements are not null).
5. **Grouping.** Methods with equal canonical expressions (at least 10 nodes, so one-liners do not match) form a class. The class lists the rules that made them equal and the conditions those rules need.

## Sound first

A wrong "these do the same thing" is the worst result this engine can give, so every rule preserves behaviour, or holds under a condition the report shows:
- "the total does not overflow int": a loop wraps where LINQ `Sum` throws;
- "the collection holds no null elements".

The corpus ([../tests/corpus/csharp](../tests/corpus/csharp/README.md)) holds near misses to keep it honest: `>=` vs `>`, `First` vs `FirstOrDefault`, `Any` vs `All`, a dropped null check, swapped arguments, integer vs decimal division, off by one, and a call kept in a local vs called twice. None may be called equal. `cargo test -p dog-food-dup --test corpus` checks it. Fields of `this` and primary-constructor parameters are matched by name and type, so the same code over an injected `_db` in two classes matches; any other member is matched by its symbol.

## What it leaves out

A method is left out, not guessed at, when it uses something the engine does not model:
- statements run only for their effect (a call, `await x;`, setting a property);
- `try`/`catch` and `using`;
- `out var`, deconstruction, `yield`;
- loops other than the folds, searches and flags above;
- expressions past 3,000 nodes.

Methods that return nothing (`void`) are not compared. `--explain` prints why each method was left out. On a 3,600-method ASP.NET Core backend, about 40% of methods reduce (most of the rest set state or await for effect), in about 30 seconds.

Next ([the plan](../../../docs/research/csharp-semantic-duplication.md)): dependence graphs for reordered and interleaved statements, and confirming matches by running both methods on generated inputs, as the corpus check does.
