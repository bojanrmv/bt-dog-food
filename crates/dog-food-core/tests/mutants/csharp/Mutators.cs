// settings: level=advanced ignore_methods=*Log*,ToString,ConfigureAwait
using System;
using System.Collections.Generic;
using System.Diagnostics.CodeAnalysis;
using System.Linq;
using System.Text.RegularExpressions;
using System.Threading.Tasks;

namespace Fixture;

public record Order(int Id, decimal Total, string? Note);

public class Holder
{
    // mutant: string "" -> "dgf was here!"
    public string Name { get; set; } = "";
}

public class Logger
{
    public void LogInformation(string message, params object[] values) { }
}

public class Mutators
{
    private readonly Logger _logger = new Logger();
    private const int Max = 2 + 1;
    private int _count;

    public int Arithmetic(int a, int b)
    {
        // mutant: arithmetic + -> -
        // mutant: arithmetic * -> /
        return a + b * 2;
    }

    public string Concatenation(string s, int a)
    {
        // mutant: arithmetic - -> +
        // mutant: string "n=" -> ""
        return "n=" + s + (a - 1);
    }

    public bool Relational(int a)
    {
        // mutant: relational > -> <
        // mutant: relational > -> >=
        // mutant: relational <= -> >
        // mutant: relational <= -> <
        // mutant: logical && -> ||
        return a > 1 && a <= 9;
    }

    public string Equality(object? o, int a)
    {
        // mutant: condition a == 1 -> false
        // mutant: equality == -> !=
        if (a == 1)
        {
            // mutant: string "one" -> ""
            return "one";
        }
        // mutant: condition o is null || a > 2 -> false
        // mutant: negate o is null || a > 2 -> !(o is null || a > 2)
        // mutant: equality null -> not null
        // mutant: logical || -> &&
        // mutant: relational > -> <
        // mutant: relational > -> >=
        if (o is null || a > 2)
        {
            // mutant: string "big" -> ""
            return "big";
        }
        // mutant: string "" -> "dgf was here!"
        return "";
    }

    public int Unary(int a, int b, bool flag)
    {
        // mutant: unary - -> +
        // mutant: arithmetic + -> -
        // mutant: unary ~b -> b
        var x = -a + ~b;
        // mutant: update ++ -> --
        // mutant: statement x++; -> ;
        x++;
        // mutant: update -- -> ++
        // mutant: statement --x; -> ;
        --x;
        // mutant: assignment += -> -=
        x += b;
        // mutant: assignment &= -> |=
        // mutant: assignment &= -> ^=
        // mutant: relational > -> <
        // mutant: relational > -> >=
        flag &= b > 0;
        // mutant: negation !flag -> flag
        // mutant: conditional !flag -> true
        // mutant: conditional !flag -> false
        return !flag ? x : 0;
    }

    public string Coalesce(string? s, string? t, int? n)
    {
        // mutant: nullcoalescing s ?? t -> t ?? s
        // mutant: nullcoalescing s ?? t -> t
        // mutant: nullcoalescing s ?? t -> s
        string? a = s ?? t;
        // mutant: nullcoalescing s ?? "x" -> "x"
        // mutant: string "x" -> ""
        string b = s ?? "x";
        // mutant: nullcoalescing t ?? throw new ArgumentNullException(nameof(t)) -> t
        string c = t ?? throw new ArgumentNullException(nameof(t));
        // mutant: assignment ??= -> =
        // mutant: string "y" -> ""
        s ??= "y";
        // mutant: nullcoalescing n ?? 0 -> 0
        var d = n ?? 0;
        return a + b + c + d;
    }

    public void Statements(List<int> items)
    {
        foreach (var item in items)
        {
            // mutant: condition item < 0 -> false
            // mutant: negate item < 0 -> !(item < 0)
            // mutant: relational < -> >
            // mutant: relational < -> <=
            if (item < 0)
            {
                // mutant: statement break; -> ;
                break;
            }
            // mutant: condition item == 0 -> false
            // mutant: equality == -> !=
            if (item == 0)
            {
                // mutant: statement continue; -> ;
                continue;
            }
            // mutant: statement Save(item); -> ;
            Save(item);
        }
        // mutant: condition items.Count == 0 -> false
        // mutant: equality == -> !=
        if (items.Count == 0)
        {
            // mutant: statement return; -> ;
            return;
        }
        // mutant: statement items.Clear(); -> ;
        items.Clear();
    }

    public int Throws(int a)
    {
        // mutant: condition a < 0 -> false
        // mutant: negate a < 0 -> !(a < 0)
        // mutant: relational < -> >
        // mutant: relational < -> <=
        if (a < 0)
        {
            // mutant: statement throw new ArgumentException("negative"); -> ;
            // mutant: string "negative" -> ""
            throw new ArgumentException("negative");
        }
        throw new InvalidOperationException();
    }

    public IEnumerable<int> Iterator(int a)
    {
        // mutant: statement yield return a; -> ;
        yield return a;
        // mutant: statement yield break; -> ;
        yield break;
    }

    public int Switch(int a)
    {
        switch (a)
        {
            case 1:
                return 10;
            // mutant: relational > -> <
            // mutant: relational > -> >=
            case > 5:
                // mutant: statement Save(a); -> ;
                Save(a);
                break;
        }
        // mutant: relational < -> >
        // mutant: relational < -> <=
        // mutant: unary - -> +
        return a switch { < 0 => -1, _ => 1 };
    }

    public int Count
    {
        // mutant: block { return _count; } -> {return default;}
        get { return _count; }
        // mutant: block { _count = value; } -> {}
        set { _count = value; }
    }

    public void Blocks(bool flag)
    {
        // mutant: condition flag -> false
        // mutant: negate flag -> !(flag)
        // mutant: block { _count = 5; } -> {}
        if (flag) { _count = 5; }
        // mutant: block { _count = 6; } -> {}
        else { _count = 6; }
    }

    public string Strings(string s)
    {
        // mutant: string @"a\b" -> ""
        var a = @"a\b";
        // mutant: string """raw""" -> ""
        var b = """raw""";
        // mutant: string $"{s}!" -> $""
        var c = $"{s}!";
        // mutant: string string.Empty -> "dgf was here!"
        var d = string.Empty;
        // mutant: string string.IsNullOrEmpty(s) -> (s != null)
        // mutant: string string.IsNullOrEmpty(s) -> (s != "")
        var e = string.IsNullOrEmpty(s);
        // mutant: string string.IsNullOrWhiteSpace(s) -> (s != null)
        // mutant: string string.IsNullOrWhiteSpace(s) -> (s != "")
        // mutant: string string.IsNullOrWhiteSpace(s) -> (s.Trim() != "")
        var f = string.IsNullOrWhiteSpace(s);
        // mutant: string "abc"u8 -> ""u8
        var g = "abc"u8.Length;
        // mutant: stringmethod StartsWith -> EndsWith
        // mutant: string "x" -> ""
        var h = s.StartsWith("x");
        // mutant: stringmethod s.Trim() -> ""
        var i = s.Trim();
        return a + b + c + d + e + f + h + i + g;
    }

    public decimal Linq(List<Order> orders, IEnumerable<int> ints)
    {
        // mutant: linq First -> FirstOrDefault
        // mutant: relational > -> <
        // mutant: relational > -> >=
        var first = orders.Where(o => o.Total > 5).First();
        // mutant: linq Any -> All
        // mutant: relational > -> <
        // mutant: relational > -> >=
        var any = orders.Any(o => o.Id > 0);
        var none = orders.Any();
        // mutant: linq Count -> Sum
        var count = ints.Count();
        // mutant: relational > -> <
        // mutant: relational > -> >=
        var positive = ints.Count(i => i > 0);
        // mutant: linq OrderBy -> OrderByDescending
        // mutant: linq Last -> First
        var last = orders.OrderBy(o => o.Id).Last();
        // mutant: conditional any -> true
        // mutant: conditional any -> false
        // mutant: arithmetic + -> -
        // mutant: arithmetic + -> -
        return any ? first.Total + last.Total : count + positive;
    }

    public decimal ChainedNull(Order? order, List<Order>? orders)
    {
        // mutant: linq First -> FirstOrDefault
        // mutant: nullcoalescing orders?.First().Total ?? 0 -> 0
        return orders?.First().Total ?? 0;
    }

    public object Initializers(int a)
    {
        // mutant: initializer { 1, 2 } -> { }
        var list = new List<int> { 1, 2 };
        // mutant: initializer { Name = "n" } -> { }
        // mutant: string "n" -> ""
        var holder = new Holder { Name = "n" };
        // mutant: initializer { 1, a } -> { }
        var array = new int[] { 1, a };
        var implicitArray = new[] { 1 };
        // mutant: checked checked(a + 1) -> a + 1
        // mutant: arithmetic + -> -
        var sum = checked(a + 1);
        // An array initializer without `new`: switched as `new int[] { }`.
        // mutant: initializer { a, 3 } -> { }
        int[] bare = { a, 3 };
        // mutant: initializer { "x" } -> { }
        // mutant: string "x" -> ""
        string[]? names = { "x" };
        // mutant: initializer { { 1, 2 }, { 3, 4 } } -> { }
        int[,] grid = { { 1, 2 }, { 3, 4 } };
        return (list, holder, array, implicitArray, sum, bare, names, grid);
    }

    // mutant: initializer { 1, 2 } -> { }
    private static readonly int[] Days = { 1, 2 };

    public bool Patterns(object o, int a)
    {
        // mutant: equality Order -> not Order
        var isOrder = o is Order;
        // mutant: equality not Order -> Order
        var isNot = o is not Order;
        // mutant: equality > 5 and < 10 -> not (> 5 and < 10)
        // mutant: relational > -> <
        // mutant: relational > -> >=
        // mutant: relational < -> >
        // mutant: relational < -> <=
        // mutant: logical and -> or
        var inRange = a is > 5 and < 10;
        // mutant: logical && -> ||
        // mutant: logical && -> ||
        return isOrder && isNot && inRange;
    }

    public bool Dates(DateOnly lastDay, DateOnly today, int days, DateTime createdAt)
    {
        // mutant: relational <= -> > (boundary-risk)
        // mutant: relational <= -> < (boundary-risk)
        var active = today <= lastDay;
        // mutant: relational > -> <
        // mutant: relational > -> >=
        var longStay = days > 30;
        // mutant: relational < -> > (boundary-risk)
        // mutant: relational < -> <= (boundary-risk)
        var future = DateTime.UtcNow < createdAt;
        // dgf-disable-once relational: the first day counts, test: PricingTests.StartsOnFirstDay
        // mutant: relational >= -> < (boundary-risk) (ignored)
        // mutant: relational >= -> > (boundary-risk) (ignored)
        var started = today >= lastDay;
        // mutant: logical && -> ||
        // mutant: logical && -> ||
        // mutant: logical && -> ||
        return active && longStay && future && started;
    }

    public decimal Declarations(object o, string s)
    {
        // mutant: condition o is Order order && order.Total > 5 -> false
        // mutant: relational > -> <
        // mutant: relational > -> >=
        // mutant: block { return order.Total; } -> {return default;}
        if (o is Order order && order.Total > 5) { return order.Total; }
        // mutant: relational > -> <
        // mutant: relational > -> >=
        var parsed = int.TryParse(s, out var n) && n > 0;
        // mutant: block { return 0; } -> {return default;}
        // mutant: condition !int.TryParse(s, out var v) -> false
        // mutant: negation !int.TryParse(s, out var v) -> int.TryParse(s, out var v)
        if (!int.TryParse(s, out var v)) { return 0; }
        // mutant: conditional parsed -> true
        // mutant: conditional parsed -> false
        // mutant: arithmetic + -> -
        return parsed ? v + n : 0;
    }

    // Patterns where some mutants cannot compile: only those mutants are left
    // out, the rest of the function is mutated.
    public string Guarded(Dictionary<string, string> metadata)
    {
        // mutant: negation !metadata.TryGetValue("id", out var raw) -> metadata.TryGetValue("id", out var raw)
        // mutant: string "id" -> ""
        // mutant: negation !Guid.TryParse(raw, out var id) -> Guid.TryParse(raw, out var id)
        if (!metadata.TryGetValue("id", out var raw) || !Guid.TryParse(raw, out var id))
        {
            // mutant: string "none" -> ""
            return "none";
        }
        return id.ToString();
    }

    public int Caught(string s)
    {
        int value;
        try
        {
            value = int.Parse(s);
        }
        catch (FormatException)
        {
            // mutant: string "bad" -> ""
            throw new ArgumentException("bad", nameof(s));
        }
        // mutant: arithmetic * -> /
        return value * 2;
    }

    public bool Parsed(string s, out int value)
    {
        try
        {
            value = int.Parse(s);
            // mutant: boolean true -> false
            return true;
        }
        catch (FormatException)
        {
            value = 0;
            // mutant: boolean false -> true
            return false;
        }
    }

    public int Spin(Queue<int> queue)
    {
        while (true)
        {
            var next = queue.Dequeue();
            // mutant: condition next > 10 -> false
            // mutant: negate next > 10 -> !(next > 10)
            // mutant: relational > -> <
            // mutant: relational > -> >=
            // mutant: block { return next; } -> {return default;}
            if (next > 10) { return next; }
        }
    }

    public decimal Fallback(decimal? price, decimal fallback)
    {
        // mutant: nullcoalescing price ?? fallback -> fallback
        var total = price ?? fallback;
        // mutant: conditional total > 0 -> true
        // mutant: conditional total > 0 -> false
        // mutant: relational > -> <
        // mutant: relational > -> >=
        return total > 0 ? total : 0;
    }

    public bool Regexes(string s)
    {
        // mutant: regex @"^\d+$" -> @"\d+$"
        // mutant: regex @"^\d+$" -> @"^[\d\D]+$"
        // mutant: regex @"^\d+$" -> @"^\D+$"
        // mutant: regex @"^\d+$" -> @"^\d$"
        // mutant: regex @"^\d+$" -> @"^\d+"
        // mutant: regex @"^\d+$" -> @"^\d+?$"
        // mutant: regex @"^\d+$" -> @"^\d{0,}$"
        // mutant: regex @"^\d+$" -> @"^\d{2,}$"
        // mutant: regex @"^\d+$" -> @"^d+$"
        var digits = new Regex(@"^\d+$");
        // mutant: regex "(a)[^b]" -> "(?:a)[^b]"
        // mutant: regex "(a)[^b]" -> "(a)[\\w\\W]"
        // mutant: regex "(a)[^b]" -> "(a)[b]"
        var named = new Regex(options: RegexOptions.None, pattern: "(a)[^b]");
        // Regex's static methods too.
        // mutant: logical || -> &&
        // mutant: logical || -> &&
        // mutant: regex "^x{2,3}" -> "^x"
        // mutant: regex "^x{2,3}" -> "^x{1,3}"
        // mutant: regex "^x{2,3}" -> "^x{2,2}"
        // mutant: regex "^x{2,3}" -> "^x{2,3}?"
        // mutant: regex "^x{2,3}" -> "^x{2,4}"
        // mutant: regex "^x{2,3}" -> "^x{3,3}"
        // mutant: regex "^x{2,3}" -> "x{2,3}"
        // mutant: string "^x{2,3}" -> ""
        return digits.IsMatch(s) || Regex.IsMatch(s, "^x{2,3}") || named.IsMatch(s);
    }

    // Copies that keep their line breaks: a string spanning lines, an `#if`.
    public Order Lines(List<Order> orders)
    {
        // mutant: equality == -> !=
        // mutant: linq First -> FirstOrDefault
        // mutant: string @"first second" -> ""
        return orders.First(o => o.Note == @"first
second");
    }

    public int Directive(List<int> values)
    {
        // mutant: linq First -> FirstOrDefault
        return values.First(v =>
#if DEBUG
            // mutant: relational > -> <
            // mutant: relational > -> >=
            v > 1
#else
            v > 2
#endif
        );
    }

    // Two bodies only parse one at a time: the first branch is mutated.
    public static int Body(int a) =>
#if NET6_0_OR_GREATER
        // mutant: arithmetic + -> -
        a + 2;
#else
        a - 2;
#endif

    public async Task<int> Ignored(int a)
    {
        // mutant: statement _logger.LogInformation("value {A}", a > 1); -> ; (ignored)
        // mutant: string "value {A}" -> "" (ignored)
        // mutant: relational > -> < (ignored)
        // mutant: relational > -> >= (ignored)
        _logger.LogInformation("value {A}", a > 1);
        // mutant: statement await Task.Delay(a > 1 ? 1 : 2).ConfigureAwait(false); -> ; (ignored)
        // mutant: relational > -> < (ignored)
        // mutant: relational > -> >= (ignored)
        // mutant: conditional a > 1 -> true (ignored)
        // mutant: conditional a > 1 -> false (ignored)
        // mutant: boolean false -> true (ignored)
        await Task.Delay(a > 1 ? 1 : 2).ConfigureAwait(false);
        // dgf-disable-once relational: the sign is all that matters here
        // mutant: relational < -> > (ignored)
        // mutant: relational < -> <= (ignored)
        // mutant: conditional a < 0 -> true
        // mutant: conditional a < 0 -> false
        // mutant: unary - -> +
        // mutant: arithmetic - -> +
        return a < 0 ? -1 : a - 1;
    }

    [ExcludeFromCodeCoverage]
    public int Excluded(int a)
    // mutant: block {... -> {return default;} (ignored)
    {
        // mutant: arithmetic + -> - (ignored)
        return a + 1;
    }

    public void Lambdas(List<int> items)
    {
        // mutant: statement items.ForEach(i => _count += i); -> ;
        items.ForEach(i => _count += i);
    }

    // mutant: arithmetic * -> /
    // mutant: arithmetic + -> -
    public int Arrow(int a) => a * 2 + Max;

    public void VoidArrow() => _count++;

    private void Save(int value) => _count = value;

    // A block that already returns null or default is not replaced by {return default;}.
    public string? Missing(bool known)
    {
        // mutant: condition known -> false
        // mutant: negate known -> !(known)
        if (known)
        {
            return null;
        }
        return default;
    }
}
