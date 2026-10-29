# dgf-dup-cs: C# that does the same thing, written differently

`dgf-dup` finds copies: code with the same shape. This engine finds C# methods that compute the same result even when the code looks different. Examples are a `for` loop and its LINQ, early returns and nested `if`s, `?:` and a `switch` expression, and `x == null` and `x is null`. It reduces every method to one canonical expression with the compiler (Roslyn), and methods with equal expressions are reported together as type 4, with evidence `normalized`.

```powershell
# install (once): a .NET tool
dotnet pack crates/dog-food-dup/semantic -c Release
dotnet tool install -g dgf-dup-cs --add-source crates/dog-food-dup/semantic/nupkg

dgf-dup --semantic                       # both engines, one report (any format, --changes too)
dgf-dup-cs src -o semantic.json          # the engine alone, dgf-dup's JSON
dgf-dup-cs src --explain -o NUL          # every method's canonical form, or why it was left out
dgf-dup-cs src --errors --timings -o NUL # the compile errors that leave methods out, and each stage's time
dgf-dup-cs src --confirm -o semantic.json   # run each group on generated inputs (runs your code)
dgf-dup-cs src --compare Orders.Total Orders.Sum   # two methods run on the same inputs: 0 agree, 1 differ, 2 not run
```

`dgf-dup --semantic` runs `dgf-dup-cs` from PATH, `DGF_DUP_CS`, or `--semantic-engine` (an executable or its `.dll`). A class the syntactic engine already reports for the same functions is not repeated.

## How it works

1. **The projects as they build** ([Projects.cs](Projects.cs)). Each `.csproj` holding a scanned file is compiled from every `.cs` file under it, against:
   - the running .NET's assemblies;
   - its packages (from `obj/project.assets.json`);
   - ASP.NET Core's shared framework when it or a project it references uses it;
   - the projects it references, transitively, as MSBuild makes them.

   The usings are its SDK's implicit ones (web and worker SDKs add theirs) and its `<Using>` items. Where two assemblies have one name, the newer wins: an old package of an ASP.NET Core part does not hide the framework's own. Files in no project are compiled together. No build is needed, only a restore. Generated files, migrations, `bin`/`obj` and (unless `--tests`) tests are left out.

   Most methods are left out, and many only at their last statement, so a syntax check ([Precheck.cs](Precheck.cs)) turns down before binding those that return nothing or hold a statement the engine does not run. The rest bind one at a time on every core, the biggest first.
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

## Repeated queries

[Queries.cs](Queries.cs) reads every LINQ chain on `IQueryable<T>` in every method: the entity `T` (the table), the filter (its `Where`s and the predicate of a last step such as `FirstOrDefaultAsync`, split at `&&` and sorted, as SQL `AND` does not care about order), and its shape (includes, ordering, projection). Values from outside the query's own lambdas become holes, which a shared version takes as parameters. A correlated subquery's `r.MemberId` is such a value too. [QueryGroups.cs](QueryGroups.cs) reports the same filter, the same shaped query, and one condition repeated across different queries of a table, each with an extension method for `{Entity}Queries` built from the first query's source. [../docs/duplication.md](../../../docs/duplication.md#repeated-queries-on-a-table) has the details. `tests/queries/Shop.cs` is the fixture.

## Confirming by running

With `--confirm` ([Confirm.cs](Confirm.cs)), each group is run.
- **Compile.** The projects of the groups are compiled in memory, in parallel, with the SDK's source generators run (`[GeneratedRegex]`, `[LoggerMessage]`, JSON, options), and loaded into their own context.
- **Inputs.** Every method of a group runs on the same 1,000 inputs, made from the parameter types and their nullability: numbers, strings, enums, dates, lists, records and classes, through their widest constructor and settable properties. Every fourth input is plain (lists of none or one, numbers 0, 1 and the methods' own constants), so a sum lands exactly on a threshold. Other inputs use the constants and the values either side of them, where `>=` and `>` part.
- **Comparing.** Results are compared by value: decimals without trailing zeros, sequences element by element, objects by their public members, exceptions by type.
- **Verdict.** An input that tells two methods apart disproves the group: it is left out of the report and listed under `disproved`. A group whose inputs mostly throw is not counted as confirmed.
- **What runs.** Only methods without effects. Calls into the project's own code are followed into the callee's canonical form, four deep. Constructors of its own types must only store what they are given. The framework's files, network, console, processes, threads and reflection, database reads, awaits, and the clock and `Guid.NewGuid` (which differ between runs) are not run, with the reason in the report.
- **Instances.** An instance method runs on an object made without its constructor, with each field holding a value generated from its name, so `_rate` in one class and `rate` in another hold the same.

On the corpus every group is confirmed, and `--compare` disproves every near miss it can run, mostly on the first inputs.

## Extracting a copy

With `--regions`, after its report the engine reads `[{file, startLine, endLine}]` on stdin and writes what each region takes and gives as a method ([Regions.cs](Regions.cs)). This comes from Roslyn's data-flow analysis:
- the parameters: locals and parameters read inside that were set before;
- the returns: those set inside that are read after;
- the jumps out;
- whether it needs `this`.

`dgf-dup --semantic` sends its syntactic C# copies this way, so their advice gets an exact signature without a second compile.

Next ([the plan](../../../docs/research/csharp-semantic-duplication.md)): Z3 for small pure functions, where generated inputs could miss a case.
