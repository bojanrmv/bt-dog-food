// C# 12 to 15 (.NET 11): primary constructors, `field`, params collections,
// collection expression arguments, union types, closed hierarchies, labeled
// break and continue, null-conditional assignment, extension blocks and
// indexers. tree-sitter-c-sharp 0.23 reads the last four as errors; dgf parses
// the file again in a form it can read (lang/prepare.rs). Compiled with every
// mutant switched in by tests/mutate_dotnet.rs when a .NET 11 SDK is installed.
using System;
using System.Collections.Generic;
using System.Linq;

namespace Modern;

public record class Cat(string Name);
public record class Dog(string Name);
public union Pet(Cat, Dog);

public closed record class GateState;
public record class Shut : GateState;
public record class Open(float Percent) : GateState;

public class Service(int limit)
{
    // mutant: conditional value > limit -> true
    // mutant: conditional value > limit -> false
    // mutant: relational > -> <
    // mutant: relational > -> >=
    public int Limit { get; set => field = value > limit ? limit : value; }

    public int Sum(params ReadOnlySpan<int> values)
    {
        var total = 0;
        // mutant: assignment += -> -=
        foreach (var v in values) { total += v; }
        // mutant: conditional total > limit -> true
        // mutant: conditional total > limit -> false
        // mutant: relational > -> <
        // mutant: relational > -> >=
        return total > limit ? limit : total;
    }

    public List<string> Names(string[] values)
    {
        // mutant: arithmetic * -> /
        List<string> names = [with(capacity: values.Length * 2), .. values];
        // mutant: conditional names.Count > 1 -> true
        // mutant: conditional names.Count > 1 -> false
        // mutant: relational > -> <
        // mutant: relational > -> >=
        return names.Count > 1 ? names : [];
    }

    public string Name(Pet pet) => pet switch
    {
        Dog d => d.Name,
        // mutant: conditional c.Name.Length > 3 -> true
        // mutant: conditional c.Name.Length > 3 -> false
        // mutant: relational > -> <
        // mutant: relational > -> >=
        // mutant: string "short" -> ""
        Cat c => c.Name.Length > 3 ? c.Name : "short",
    };

    public string Describe(GateState state) => state switch
    {
        // mutant: string "closed" -> ""
        Shut => "closed",
        // mutant: conditional percent > 50 -> true
        // mutant: conditional percent > 50 -> false
        // mutant: relational > -> <
        // mutant: relational > -> >=
        // mutant: string "wide" -> ""
        // mutant: string "ajar" -> ""
        Open(var percent) => percent > 50 ? "wide" : "ajar",
    };

    public int Find(int[][] grid)
    {
        var found = 0;
        // mutant: relational < -> >
        // mutant: relational < -> <=
        outer: for (int row = 0; row < grid.Length; row++)
        {
            // mutant: relational < -> >
            // mutant: relational < -> <=
            // mutant: update ++ -> --
            for (int column = 0; column < grid[row].Length; column++)
            {
                // mutant: condition grid[row][column] < 0 -> false
                // mutant: negate grid[row][column] < 0 -> !(grid[row][column] < 0)
                // mutant: relational < -> >
                // mutant: relational < -> <=
                // mutant: statement continue outer; -> ;
                if (grid[row][column] < 0) { continue outer; }
                // mutant: condition grid[row][column] == 9 -> false
                // mutant: equality == -> !=
                // mutant: statement found++; -> ;
                // mutant: update ++ -> --
                // mutant: statement break outer; -> ;
                if (grid[row][column] == 9) { found++; break outer; }
            }
        }
        return found;
    }

    public void Assign(Holder? holder, int value)
    {
        // mutant: conditional value > 0 -> true
        // mutant: conditional value > 0 -> false
        // mutant: relational > -> <
        // mutant: relational > -> >=
        holder?.Value = value > 0 ? value : 0;
    }
}

public class Holder { public int Value { get; set; } }

public static class Extensions
{
    extension(IEnumerable<int> sequence)
    {
        public int this[int index] => sequence.ElementAt(index);
        // mutant: linq Count -> Sum
        // mutant: relational > -> <
        // mutant: relational > -> >=
        public bool HasMany => sequence.Count() > 1;
        // mutant: arithmetic * -> /
        public int Doubled(int index) => sequence.ElementAt(index) * 2;
    }
}
