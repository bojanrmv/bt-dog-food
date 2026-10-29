using System.Collections.Immutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Operations;

namespace DgfDup;

/// <summary>
/// Calls: LINQ in the shared vocabulary of collections, the framework
/// members whose meaning the rules know, values being built (a
/// <c>StringBuilder</c>, a list, a dictionary) and every other call as
/// itself, by its method.
/// </summary>
public static class Calls
{
    public static Node Invocation(IInvocationOperation op, Scope s)
    {
        var method = op.TargetMethod;
        var owner = method.ContainingType?.ToDisplayString();
        if (owner == "System.Linq.Enumerable" && Linq.TryGetValue(method.Name, out var linq))
        {
            return linq(op, s);
        }
        if (Known.TryGetValue(owner + "." + method.Name, out var known))
        {
            return known(op, s);
        }
        return OnBuilt(op, s) ?? Plain(op, s);
    }

    /// <summary>A method of a value being built, called on it.</summary>
    static Node? OnBuilt(IInvocationOperation op, Scope s) =>
        op.Instance is null ? null : Built(Lower.Expr(op.Instance, s), op.TargetMethod, op, s);

    /// <summary>A call as itself: its method, its receiver and its arguments.</summary>
    static Node Plain(IInvocationOperation op, Scope s)
    {
        var receiver = op.Instance is null ? null : Lower.Expr(op.Instance, s);
        var args = op.Arguments.Select(a => Lower.Expr(a.Value, s));
        var all = (receiver is null ? args : args.Prepend(receiver)).ToImmutableArray();
        return Canon.Make("call:" + Types.Symbol(op.TargetMethod.OriginalDefinition), all);
    }

    static Node Source(IInvocationOperation op, Scope s) => Lower.Expr(op.Arguments[0].Value, s);

    /// <summary>The lambda in argument <paramref name="index"/>, or <c>x => x</c> without one.</summary>
    static Node Selector(IInvocationOperation op, int index, Scope s) =>
        op.Arguments.Length > index ? Lower.Expr(op.Arguments[index].Value, s) : Canon.Lambda($"lambda:{s.Depth}", new Node($"elem:{s.Depth}"));

    static Node? Predicate(IInvocationOperation op, Scope s) =>
        op.Arguments.Length > 1 ? Lower.Expr(op.Arguments[1].Value, s) : null;

    static Node Always => Canon.Lambda("lambda:_", Node.True);

    static string Result(IInvocationOperation op) => Types.Name(op.Type);

    static readonly Dictionary<string, Func<IInvocationOperation, Scope, Node>> Linq = new()
    {
        ["Sum"] = (op, s) => Canon.Sum(Result(op), Source(op, s), Selector(op, 1, s)),
        ["Count"] = (op, s) => Canon.Count(Source(op, s), Predicate(op, s)),
        ["Any"] = (op, s) => Canon.Any(Source(op, s), Predicate(op, s) ?? Always),
        ["All"] = (op, s) => Canon.All(Source(op, s), Predicate(op, s)!),
        ["Where"] = (op, s) => Canon.Where(Source(op, s), Predicate(op, s)!),
        ["Select"] = (op, s) => new Node("select", Source(op, s), Selector(op, 1, s)),
        ["ToList"] = (op, s) => new Node("toList", Source(op, s)),
        ["ToArray"] = (op, s) => new Node("toArray", Source(op, s)),
        ["First"] = (op, s) => Find("first", op, s),
        ["FirstOrDefault"] = (op, s) => Find("firstOrDefault", op, s),
        ["Max"] = (op, s) => Extreme("max", op, s),
        ["Min"] = (op, s) => Extreme("min", op, s),
        ["DefaultIfEmpty"] = (op, s) => new Node("defaultIfEmpty", Source(op, s), Selector(op, 1, s)),
        ["GroupBy"] = (op, s) => new Node("groupBy", Source(op, s), Selector(op, 1, s)),
        ["ToDictionary"] = ToDictionary,
    };

    /// <summary><c>First(c)</c> and <c>Where(c).First()</c> are one search.</summary>
    static Node Find(string head, IInvocationOperation op, Scope s)
    {
        var source = Source(op, s);
        var predicate = Predicate(op, s) ?? Always;
        if (source.Is("where") && predicate.Kids[0].Equals(Node.True))
        {
            return new Node(head, source.Kids[0], source.Kids[1]);
        }
        return new Node(head, source, predicate);
    }

    /// <summary><c>Max(f)</c>; over <c>DefaultIfEmpty(d)</c>, <c>d</c> when empty.</summary>
    static Node Extreme(string which, IInvocationOperation op, Scope s)
    {
        var source = Source(op, s);
        var selector = Selector(op, 1, s);
        var type = Result(op);
        if (source.Is("defaultIfEmpty") && source.Kids[1].Kids.IsEmpty)
        {
            Canon.Rule("DefaultIfEmpty(d).Max() → empty ? d : Max()");
            var inner = source.Kids[0];
            return Canon.Choose(Canon.NotEmpty(inner), Extreme(which, type, inner, selector), source.Kids[1]);
        }
        if (source.Is("defaultIfEmpty"))
        {
            var inner = source.Kids[0];
            Canon.Rule("DefaultIfEmpty(d).Max() → empty ? d : Max()");
            return Canon.Choose(Canon.NotEmpty(inner), Extreme(which, type, inner, selector), source.Kids[1].Kids[0]);
        }
        return Extreme(which, type, source, selector);
    }

    static Node Extreme(string which, string type, Node source, Node selector) =>
        which == "max" ? Canon.Max(type, source, selector) : new Node($"minOf:{type}", source, selector);

    /// <summary><c>GroupBy(k).ToDictionary(g => g.Key, g => g.Sum(v))</c> is a sum per key.</summary>
    static Node ToDictionary(IInvocationOperation op, Scope s)
    {
        var source = Source(op, s);
        if (!source.Is("groupBy") || op.Arguments.Length != 3)
        {
            return Canon.Make("toDictionary", source, Selector(op, 1, s), Selector(op, 2, s));
        }
        var key = Selector(op, 1, s);
        var value = Selector(op, 2, s);
        var group = Canon.Element(value.Head);
        var keyRead = key.Kids[0];
        var sum = value.Kids[0];
        if (!keyRead.StartsWith("member:System.Linq.IGrouping") || !sum.StartsWith("sum:") || !sum.Kids[0].Equals(group))
        {
            throw new UnsupportedException("ToDictionary of groups other than a sum per key");
        }
        Canon.Rule("GroupBy then ToDictionary(Sum) → sum per key");
        // The sum's lambda reads each item of the group; as the item it is one level out.
        var perItem = Shift(sum.Kids[1], -1);
        return new Node("groupSum:" + sum.Head["sum:".Length..], source.Kids[0], source.Kids[1], perItem);
    }

    /// <summary>Elements and lambdas at <c>level + 1</c> and deeper moved by <paramref name="by"/> levels.</summary>
    public static Node Shift(Node n, int by)
    {
        var head = n.Head;
        foreach (var prefix in new[] { "elem:", "lambda:" })
        {
            if (head.StartsWith(prefix, StringComparison.Ordinal) && int.TryParse(head[prefix.Length..], out var level) && level > 0)
            {
                head = prefix + (level + by);
            }
        }
        return new Node(head, n.Kids.Select(k => Shift(k, by)).ToImmutableArray());
    }

    static readonly Dictionary<string, Func<IInvocationOperation, Scope, Node>> Known = new()
    {
        ["System.Math.Max"] = (op, s) => MinMax("max", op, s),
        ["System.Math.Min"] = (op, s) => MinMax("min", op, s),
        ["string.IsNullOrEmpty"] = (op, s) => Canon.NullOrEmpty(Lower.Expr(op.Arguments[0].Value, s)),
        ["System.Collections.Generic.CollectionExtensions.GetValueOrDefault"] = ValueOrDefault,
    };

    static Node MinMax(string which, IInvocationOperation op, Scope s)
    {
        var a = Lower.Expr(op.Arguments[0].Value, s);
        var b = Lower.Expr(op.Arguments[1].Value, s);
        var type = Types.Name(op.Type);
        return string.CompareOrdinal(a.Key, b.Key) <= 0 ? Canon.Make($"{which}:{type}", a, b) : Canon.Make($"{which}:{type}", b, a);
    }

    /// <summary>
    /// A method of a value being built: <c>sb.Append(x)</c> adds <c>x</c>'s
    /// text, <c>sb.ToString()</c> is the text; <c>list.Add(x)</c> appends;
    /// <c>d.GetValueOrDefault(k)</c> reads or gives the default.
    /// </summary>
    static Node? Built(Node target, IMethodSymbol method, IInvocationOperation op, Scope s)
    {
        var kind = Kind(target);
        if (kind is null)
        {
            return null;
        }
        var args = op.Arguments.Select(a => Lower.Expr(a.Value, s)).ToArray();
        return (kind, method.Name, args.Length) switch
        {
            ("builder", "Append", 1) => new Node("builder", Canon.Concat([target.Kids[0], Canon.Part(args[0])])),
            ("builder", "ToString", 0) => target.Kids[0],
            ("list", "Add", 1) => new Node("append", target, args[0]),
            _ => throw new UnsupportedException($"{method.Name} on a {kind} being built"),
        };
    }

    /// <summary><c>dict.GetValueOrDefault(k)</c>: the value, or the type's default.</summary>
    static Node ValueOrDefault(IInvocationOperation op, Scope s)
    {
        var dict = Lower.Expr(op.Arguments[0].Value, s);
        var key = Lower.Expr(op.Arguments[1].Value, s);
        if (Kind(dict) != "dict" || op.Arguments.Length != 2)
        {
            return Canon.Make("call:" + Types.Symbol(op.TargetMethod.OriginalDefinition), dict, key);
        }
        var type = Types.Name(op.Type);
        var fallback = Numbers.IsNumber(type) ? Node.Const(type, "0") : new Node("default:" + type);
        return Canon.Choose(Has(dict, key), Get(dict, key), fallback);
    }

    /// <summary>What a value being built is: <c>dict</c>, <c>list</c> or <c>builder</c>; null for anything else.</summary>
    public static string? Kind(Node n) =>
        BuiltKinds.TryGetValue(n.Head, out var kind) ? kind : n.StartsWith("acc:") ? PlaceholderKind(n) : null;

    static readonly Dictionary<string, string> BuiltKinds = new()
    {
        ["dict"] = "dict",
        ["set"] = "dict",
        ["list"] = "list",
        ["append"] = "list",
        ["builder"] = "builder",
    };

    /// <summary>A loop's placeholder <c>acc:kind:n</c> names the kind of value it stands for.</summary>
    static string? PlaceholderKind(Node n)
    {
        var kind = n.Head.Split(':')[1];
        return kind == "value" ? null : kind;
    }

    public static Node Has(Node dict, Node key) => Canon.Atom(new Node("has", dict, key));

    public static Node Get(Node dict, Node key) => new("get", dict, key);
}
