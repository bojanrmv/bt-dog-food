# dgf-dup-cs

**Finds C# methods that do the same thing written differently**, such as a `foreach` that adds up and its `.Sum(...)`, or an `if` chain and a `switch`. It also finds LINQ queries that repeat a filter on one table, and suggests the extension method that replaces them. It is built on the C# compiler (Roslyn).

**Most people do not need to install this package.** It is the C# engine of [`dgf`](https://www.nuget.org/packages/dgf), the code-health tool for complexity, mutation testing and duplication, and `dotnet tool install -g dgf` already includes it. Install it on its own when you built `dgf` from source with cargo, or to run the engine directly.

You need the [.NET 10 SDK](https://dotnet.microsoft.com/download/dotnet/10.0).

```powershell
dotnet tool install -g dgf-dup-cs

dgf dup                                            # with dgf: both duplication engines, one report
dgf-dup-cs src --compare Orders.Total Orders.Sum   # on its own: do these two methods agree?
```

## How it decides

Each method is run symbolically to the value it returns, and that value is written in one normal form, so loops and LINQ, `if` and `?:`, and `x == null` and `x is null` come out the same. Methods with the same form are reported together, with the rules that made them equal.

It avoids false matches: `>=` and `>`, `First` and `FirstOrDefault`, a dropped null check or swapped arguments are never called equal, and a method it cannot model is left out rather than guessed at. `dgf dup --confirm` also runs each group on generated inputs, and drops any group an input tells apart.

## Documentation

- [The C# engine](https://github.com/bojanrmv/bt-dog-food/blob/main/docs/duplication/csharp.md)
- [Repeated queries](https://github.com/bojanrmv/bt-dog-food/blob/main/docs/duplication/queries.md)
- [The dog-food project](https://github.com/bojanrmv/bt-dog-food#readme)

MIT licensed.
