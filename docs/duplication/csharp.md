# The C# engine

[Duplication](README.md) › The C# engine

`dgf-dup-cs` is a .NET tool built on the C# compiler (Roslyn). It finds methods that compute the same result even when the code looks different, reads every LINQ query on `IQueryable<T>` ([repeated queries](queries.md)), and gives the first copy of each syntactic C# group an exact signature. It runs at the same time as the syntactic engine.

```powershell
dotnet tool install -g dgf-dup-cs   # only with dgf built from source: the dgf package includes it

dgf dup                            # both engines, one report
dgf dup --confirm                  # also run each type-4 group on generated inputs
```

The engine is found as `--semantic-engine PATH`, else the `DGF_DUP_CS` variable, else `engine/` beside the `dgf` binary, following a link to it (where the NuGet package and the release downloads put it), else `dgf-dup-cs` on the PATH. Installed with `dotnet tool install -g dgf`, it is already there. To run a changed engine from this repository: `dotnet pack crates/dog-food-dup/semantic -c Release`, then `dotnet tool update -g dgf-dup-cs --add-source crates/dog-food-dup/semantic/nupkg`, or point `DGF_DUP_CS` at its `.dll`.

## When it runs

| Command | The C# engine |
| --- | --- |
| `dgf dup` with C# to scan | runs. If it cannot, a `note:` in the report says what is missing and why, and the run carries on. |
| `dgf dup --changes` | off: it would scan the merge base as well, and a gate stays fast. So `--fail-on-copies` does not see a new type-4 copy. |
| `--semantic`, `--confirm` or `--semantic-engine` | required, `--changes` too. If it cannot run, the exit code is 2. |
| `--no-semantic` | off, with no note. |

A project with no `obj/project.assets.json` is compiled without its packages, and methods that do not bind are left out. The report's note names those projects: run `dotnet restore` for the full result.

## Type 4: the same behaviour

Each method is run symbolically to the value it returns, and that value is written in one normal form:

| Written as | Becomes |
| --- | --- |
| locals | inlined; a value with effects (a call, an `await`) is bound once |
| `if` with early returns, nested `if`s, `?:`, `switch` expressions | one decision tree |
| `a > b`, `a >= b` | one polarity (`>=` only for total orders: integers, decimals, enums, dates) |
| `for`, `foreach`, `while` over a list | what they compute: `Sum`, `Count`, `Any`, `First`, `Max`, `Where`/`Select`, a sum per key |
| `+`, interpolation, `StringBuilder` | one concatenation |
| `x == null`, `x is null`, `?.`, `??`, patterns | the tests they make |

Methods with the same expression are type 4. The finding lists the rules that made them equal, and any condition those need ("equal when the collection holds no null elements").

It is sound first. A near miss is never called equal: `>=` vs `>`, `First` vs `FirstOrDefault`, a dropped null check, swapped arguments. A method it cannot model is left out, not guessed at: statements run only for their effect, `try`/`catch`, `switch` statements, `yield`, other loops. `dgf-dup-cs src --explain -o NUL` prints every method's form, or why it was left out.

## Projects compiled as they build

- Each `.csproj` is compiled from its files, with its SDK's implicit usings, its `<Using>` items and its packages (from `obj/project.assets.json`). It needs a `dotnet restore`, not a build.
- Projects it references are included, transitively, as MSBuild does.
- Where a package and the framework carry one assembly, the newer wins.
- `dgf-dup-cs src --errors` lists compile errors that leave methods out. `--timings` shows each stage's time.

## `--confirm`: checked by running

Opt-in, because it runs your code.

- The projects of the type-4 groups are compiled in memory, with their source generators (`[GeneratedRegex]`, `[LoggerMessage]`), and loaded on their own.
- Each method of a group runs on the same 1,000 inputs. They are made from the parameter types, the methods' own constants and the values either side of them, and the usual edge values (empty lists, nulls where allowed, zero).
- Results are compared by value. An input that tells two methods apart disproves the group: it is dropped, and the input is printed.
- Only methods without effects run. Not run: database reads, files, network, console, the clock, `Guid.NewGuid`, awaits, and the project's own methods and constructors unless their bodies have no effects either. The report says why.

Two methods by name, the same way:

```powershell
dgf-dup-cs src --compare Orders.Total Orders.Sum   # exit 0 agree, 1 differ, 2 not run
```

## Exact extraction signatures

For the first copy of each syntactic C# group (not repeated statements), the compiler's data-flow analysis says what an extracted method would take and return: the values read inside that were set before (parameters), those set inside that are read after (returns), early `return`/`break`/`continue`, and whether it needs `this`.

```
as a method (from the compiler): takes (IReadOnlyList<Line> lines, string customer), returns (string heading, decimal rounded), needs this (an instance method)
```

Engine internals: [crates/dog-food-dup/semantic/README.md](../../crates/dog-food-dup/semantic/README.md). The research behind it: [csharp-semantic-duplication.md](../research/csharp-semantic-duplication.md).
