using System.Collections.Immutable;

namespace DgfDup;

/// <summary>
/// Smart constructors that keep expressions in one normal form as they are
/// built, so code written differently builds the same tree:
/// <list type="bullet">
/// <item>Booleans are decision trees: <c>cond(atom, T, F)</c> for a test,
/// <c>!</c>, <c>&amp;&amp;</c> and <c>||</c> graft trees onto each other's
/// leaves, keeping the order in which C# evaluates them.</item>
/// <item>Comparisons have one polarity: <c>a &gt; b</c> is <c>b &lt; a</c>;
/// <c>a &gt;= b</c> is <c>!(a &lt; b)</c> only for total orders (integers,
/// decimals, enums), never for floating point, where NaN breaks it.</item>
/// <item>A condition inside an expression is lifted out:
/// <c>f(c ? a : b)</c> is <c>c ? f(a) : f(b)</c>.</item>
/// <item>LINQ and the loops that do the same have one vocabulary: <c>sum</c>,
/// <c>count</c>, <c>any</c>, <c>first</c>, <c>max</c>, <c>where</c>,
/// <c>select</c>, <c>toList</c>, <c>groupSum</c>.</item>
/// </list>
/// Every rule is behaviour-preserving, or holds under a condition it records
/// (no integer overflow, no null elements), which the report shows.
/// </summary>
public static class Canon
{
    /// <summary>The rules and conditions the current method needed.</summary>
    [ThreadStatic] static Trace? trace;

    public static Trace Begin() => trace = new Trace();

    public static void Rule(string name) => trace?.Rules.Add(name);

    public static void Assume(string condition) => trace?.Conditions.Add(condition);

    // ---- booleans -------------------------------------------------------

    public static bool IsBool(Node n) =>
        n.Equals(Node.True) || n.Equals(Node.False) || (n.Is("cond") && IsBool(n.Kids[1]) && IsBool(n.Kids[2]));

    /// <summary>A test as a decision tree.</summary>
    public static Node Atom(Node test) => IsBool(test) ? test : Node.Cond(test, Node.True, Node.False);

    /// <summary>The tree with its true leaves replaced by <paramref name="whenTrue"/> and its false leaves by <paramref name="whenFalse"/>.</summary>
    public static Node Graft(Node tree, Node whenTrue, Node whenFalse)
    {
        if (tree.Equals(Node.True))
        {
            return whenTrue;
        }
        if (tree.Equals(Node.False))
        {
            return whenFalse;
        }
        if (!tree.Is("cond"))
        {
            return Choose(Atom(tree), whenTrue, whenFalse);
        }
        return Branch(tree.Kids[0], Graft(tree.Kids[1], whenTrue, whenFalse), Graft(tree.Kids[2], whenTrue, whenFalse));
    }

    public static Node Not(Node b) => Graft(Atom(b), Node.False, Node.True);

    public static Node And(Node a, Node b) => Graft(Atom(a), Atom(b), Node.False);

    public static Node Or(Node a, Node b) => Graft(Atom(a), Node.True, Atom(b));

    /// <summary><c>test ? then : otherwise</c>, for any test.</summary>
    public static Node Choose(Node test, Node then, Node otherwise)
    {
        if (IsBool(test) && test.Is("cond") || test.Equals(Node.True) || test.Equals(Node.False))
        {
            return Graft(test, then, otherwise);
        }
        return Branch(test, then, otherwise);
    }

    /// <summary>A branch on one atom: the decision-tree node itself.</summary>
    static Node Branch(Node atom, Node then, Node otherwise)
    {
        if (then.Equals(otherwise) && IsSafe(atom))
        {
            return then;
        }
        var max = MinMax(atom, then, otherwise);
        return max ?? Node.Cond(atom, then, otherwise);
    }

    /// <summary><c>a &lt; b ? b : a</c> is <c>max(a, b)</c>; <c>a &lt; b ? a : b</c> is <c>min(a, b)</c>.</summary>
    static Node? MinMax(Node atom, Node then, Node otherwise)
    {
        if (!IsLess(atom) || Picks(atom.Kids[0], atom.Kids[1], then, otherwise) is not { } kind)
        {
            return null;
        }
        Rule($"conditional → {kind}");
        return Sorted($"{kind}:{Type(atom.Head)}", atom.Kids[0], atom.Kids[1]);
    }

    /// <summary>A <c>&lt;</c> between two values of a total order.</summary>
    static bool IsLess(Node atom) => atom.StartsWith("op:<:") && atom.Kids.Length == 2 && Numbers.IsTotal(Type(atom.Head));

    /// <summary><c>max</c> when <c>a &lt; b ? b : a</c>, <c>min</c> when <c>a &lt; b ? a : b</c>.</summary>
    static string? Picks(Node a, Node b, Node then, Node otherwise)
    {
        if (then.Equals(b) && otherwise.Equals(a))
        {
            return "max";
        }
        return then.Equals(a) && otherwise.Equals(b) ? "min" : null;
    }

    /// <summary>Tests that cannot throw, so dropping one changes nothing.</summary>
    static bool IsSafe(Node atom) => atom.Kids.All(k => k.StartsWith("param:") || k.StartsWith("const:") || k.StartsWith("elem:"));

    static string Type(string head) => head[(head.LastIndexOf(':') + 1)..];

    static Node Sorted(string head, Node a, Node b) =>
        string.CompareOrdinal(a.Key, b.Key) <= 0 ? new Node(head, a, b) : new Node(head, b, a);

    // ---- comparisons ----------------------------------------------------

    /// <summary>A comparison as a decision tree with one polarity.</summary>
    public static Node Compare(string op, string type, Node a, Node b) => Comparisons[op](type, a, b);

    static readonly Dictionary<string, Func<string, Node, Node, Node>> Comparisons = new()
    {
        ["=="] = Equal,
        ["!="] = (type, a, b) => Not(Equal(type, a, b)),
        ["<"] = Less,
        [">"] = (type, a, b) => Less(type, b, a),
        [">="] = (type, a, b) => Numbers.IsTotal(type) ? Not(Less(type, a, b)) : Atom(new Node($"op:<=:{type}", b, a)),
        ["<="] = (type, a, b) => Numbers.IsTotal(type) ? Not(Less(type, b, a)) : Atom(new Node($"op:<=:{type}", a, b)),
    };

    static Node Less(string type, Node a, Node b) =>
        Folded(a, b) ?? CountBound(a, b) ?? Lifted($"op:<:{type}", a, b, (x, y) => Atom(new Node($"op:<:{type}", x, y)));

    /// <summary>Two numeric constants compared.</summary>
    static Node? Folded(Node a, Node b) =>
        Numbers.Value(a) is { } x && Numbers.Value(b) is { } y ? (x < y ? Node.True : Node.False) : null;

    /// <summary>A count is never negative: <c>0 &lt; n</c> is "not empty", <c>n &lt; 1</c> is "empty".</summary>
    static Node? CountBound(Node a, Node b)
    {
        if (Counted(b) is { } xs && IsConst(a, "0"))
        {
            return NotEmpty(xs);
        }
        return Counted(a) is { } ys && IsConst(b, "1") ? Not(NotEmpty(ys)) : null;
    }

    static Node Equal(string type, Node a, Node b) =>
        EmptyTest(a, b) ?? LiftedEqual(type, a, b) ?? ConstantsEqual(a, b) ?? NeverNullEqual(a, b) ?? NullFirst(a, b) ?? Atom(Sorted("eq", a, b));

    /// <summary><c>xs.Count == 0</c> is "empty".</summary>
    static Node? EmptyTest(Node a, Node b)
    {
        var xs = IsConst(b, "0") ? Counted(a) : IsConst(a, "0") ? Counted(b) : null;
        return xs is null ? null : Not(NotEmpty(xs));
    }

    static Node? LiftedEqual(string type, Node a, Node b) =>
        a.Is("cond") || b.Is("cond") ? Lifted("eq", a, b, (x, y) => Equal(type, x, y)) : null;

    static Node? ConstantsEqual(Node a, Node b) =>
        a.StartsWith("const:") && b.StartsWith("const:") ? (a.Equals(b) ? Node.True : Node.False) : null;

    /// <summary>A string operation's result compared with null.</summary>
    static Node? NeverNullEqual(Node a, Node b) =>
        a.Equals(Node.Null) && NeverNull(b) || b.Equals(Node.Null) && NeverNull(a) ? Node.False : null;

    /// <summary><c>xs.FirstOrDefault(c) == null</c> is "no element matches", when the elements are not null.</summary>
    static Node? NullFirst(Node a, Node b)
    {
        var other = a.Equals(Node.Null) ? b : b.Equals(Node.Null) ? a : null;
        if (other is null || !other.Is("firstOrDefault"))
        {
            return null;
        }
        Rule("FirstOrDefault == null → !Any");
        Assume("the collection holds no null elements");
        return Not(Atom(new Node("any", other.Kids)));
    }

    /// <summary>The collection when <paramref name="n"/> is its count.</summary>
    static Node? Counted(Node n) => n.Is("count") && n.Kids.Length == 1 ? n.Kids[0] : null;

    public static Node NotEmpty(Node xs)
    {
        Rule("count compared with 0 → Any");
        return Any(xs, new Node("lambda:any", Node.True));
    }

    static bool IsConst(Node n, string value) => n.StartsWith("const:") && n.Head.EndsWith(":" + value, StringComparison.Ordinal);

    /// <summary>Results the framework never makes null: literals and string operations.</summary>
    static bool NeverNull(Node n) => n.Is("concat") || NonNull.Any(prefix => n.Head.StartsWith(prefix, StringComparison.Ordinal));

    static readonly string[] NonNull = ["const:string", "call:string.Trim", "call:string.ToUpper", "call:string.ToLower", "call:string.Substring"];

    /// <summary><c>f(c ? a : b, y)</c> is <c>c ? f(a, y) : f(b, y)</c>.</summary>
    static Node Lifted(string what, Node a, Node b, Func<Node, Node, Node> make)
    {
        if (a.Is("cond"))
        {
            Rule("condition lifted out of an expression");
            return Branch(a.Kids[0], make(a.Kids[1], b), make(a.Kids[2], b));
        }
        if (b.Is("cond"))
        {
            Rule("condition lifted out of an expression");
            return Branch(b.Kids[0], make(a, b.Kids[1]), make(a, b.Kids[2]));
        }
        return make(a, b);
    }

    // ---- general expressions --------------------------------------------

    /// <summary>A node with conditions in its arguments lifted out.</summary>
    public static Node Make(string head, params Node[] kids) => Make(head, ImmutableArray.Create(kids));

    public static Node Make(string head, ImmutableArray<Node> kids)
    {
        for (var i = 0; i < kids.Length; i++)
        {
            if (kids[i].Is("cond"))
            {
                Rule("condition lifted out of an expression");
                var c = kids[i];
                return Branch(c.Kids[0], Make(head, kids.SetItem(i, c.Kids[1])), Make(head, kids.SetItem(i, c.Kids[2])));
            }
        }
        return Converted(head, kids) ?? Arithmetic(head, kids) ?? new Node(head, kids);
    }

    /// <summary>A numeric constant converted is the constant of the new type: 0 as a decimal is 0m.</summary>
    static Node? Converted(string head, ImmutableArray<Node> kids)
    {
        if (!head.StartsWith("conv:", StringComparison.Ordinal) || kids.Length != 1 || Numbers.Value(kids[0]) is not { } value)
        {
            return null;
        }
        var type = head["conv:".Length..];
        return Numbers.IsNumber(type) ? Node.Const(type, value.ToString("G29", System.Globalization.CultureInfo.InvariantCulture)) : null;
    }

    /// <summary>Commutative operators with sorted operands, and identities.</summary>
    static Node? Arithmetic(string head, ImmutableArray<Node> kids)
    {
        if (!IsNumericOperator(head, kids))
        {
            return null;
        }
        var op = head[3..head.LastIndexOf(':')];
        if (op == "*")
        {
            return Sorted(head, kids[0], kids[1]);
        }
        return op == "+" ? PlusZero(kids[0], kids[1]) ?? Sorted(head, kids[0], kids[1]) : null;
    }

    static bool IsNumericOperator(string head, ImmutableArray<Node> kids) =>
        kids.Length == 2 && head.StartsWith("op:", StringComparison.Ordinal) && Numbers.IsNumber(Type(head));

    static Node? PlusZero(Node a, Node b)
    {
        if (!IsZero(a) && !IsZero(b))
        {
            return null;
        }
        Rule("x + 0 → x");
        return IsZero(a) ? b : a;
    }

    static bool IsZero(Node n) => n.StartsWith("const:") && (n.Head.EndsWith(":0", StringComparison.Ordinal));

    // ---- strings ----------------------------------------------------------

    /// <summary>Concatenation, flattened, with neighbouring literals joined.</summary>
    public static Node Concat(IEnumerable<Node> parts)
    {
        var flat = new List<Node>();
        foreach (var part in parts)
        {
            if (part.Is("cond"))
            {
                // Lift: everything after is built per branch.
                var rest = parts.SkipWhile(p => !ReferenceEquals(p, part)).Skip(1).ToList();
                var before = flat.ToList();
                return Branch(part.Kids[0],
                    Concat(before.Append(part.Kids[1]).Concat(rest)),
                    Concat(before.Append(part.Kids[2]).Concat(rest)));
            }
            AddPart(flat, part);
        }
        return new Node("concat", flat.ToArray());
    }

    static void AddPart(List<Node> flat, Node part)
    {
        if (part.Is("concat"))
        {
            part.Kids.ToList().ForEach(kid => AddPart(flat, kid));
            return;
        }
        if (!part.StartsWith("const:string:"))
        {
            flat.Add(part);
            return;
        }
        AddText(flat, Literal(part));
    }

    /// <summary>A literal: nothing when empty, joined to a literal just before it.</summary>
    static void AddText(List<Node> flat, string text)
    {
        if (text.Length == 0)
        {
            return;
        }
        if (flat.Count > 0 && flat[^1].StartsWith("const:string:"))
        {
            flat[^1] = Node.Const("string", Quote(Literal(flat[^1]) + text));
            return;
        }
        flat.Add(Node.Const("string", Quote(text)));
    }

    /// <summary>A value as concatenation shows it: its text, or nothing for null.</summary>
    public static Node Part(Node value) =>
        value.StartsWith("const:string:") || value.Is("concat") ? value : Make("part", value);

    public static string Quote(string text) => System.Text.Json.JsonSerializer.Serialize(text);

    static string Literal(Node n) => System.Text.Json.JsonSerializer.Deserialize<string>(n.Head["const:string:".Length..])!;

    /// <summary><c>string.IsNullOrEmpty(s)</c> as the two tests it makes.</summary>
    public static Node NullOrEmpty(Node s)
    {
        if (s.Is("cond"))
        {
            return Branch(s.Kids[0], NullOrEmpty(s.Kids[1]), NullOrEmpty(s.Kids[2]));
        }
        Rule("IsNullOrEmpty → == null || Length == 0");
        return Or(Equal("string", s, Node.Null), Equal("int", Make("member:string.Length", s), Node.Const("int", "0")));
    }

    // ---- collections ------------------------------------------------------

    public static Node Any(Node xs, Node predicate)
    {
        if (xs.Is("where"))
        {
            Rule("Where then Any → Any");
            return Any(xs.Kids[0], Conjoin(xs.Kids[1], predicate));
        }
        if (xs.Is("select") && IsTrue(predicate))
        {
            Rule("Select keeps the count");
            return Any(xs.Kids[0], predicate);
        }
        return Atom(new Node("any", xs, predicate));
    }

    public static Node All(Node xs, Node predicate)
    {
        Rule("All → !Any(!)");
        return Not(Any(xs, Negate(predicate)));
    }

    public static Node Where(Node xs, Node predicate)
    {
        if (xs.Is("where"))
        {
            Rule("Where then Where → one Where");
            return Where(xs.Kids[0], Conjoin(xs.Kids[1], predicate));
        }
        return IsTrue(predicate) ? xs : new Node("where", xs, predicate);
    }

    public static Node Count(Node xs, Node? predicate)
    {
        var source = predicate is null ? xs : Where(xs, predicate);
        return new Node("count", source);
    }

    public static Node Sum(string type, Node xs, Node selector)
    {
        if (xs.Is("select"))
        {
            Rule("Select then Sum → Sum");
            return Sum(type, xs.Kids[0], Compose(xs.Kids[1], selector));
        }
        if (Numbers.IsIntegral(type))
        {
            Assume("the total does not overflow " + type);
        }
        return new Node($"sum:{type}", xs, selector);
    }

    public static Node Max(string type, Node xs, Node selector)
    {
        if (xs.Is("select"))
        {
            Rule("Select then Max → Max");
            return Max(type, xs.Kids[0], Compose(xs.Kids[1], selector));
        }
        return new Node($"maxOf:{type}", xs, selector);
    }

    static bool IsTrue(Node lambda) => Body(lambda).Equals(Node.True);

    /// <summary>A lambda's body; anything else passed where a lambda goes (a method group) is not modelled.</summary>
    public static Node Body(Node lambda) =>
        lambda.StartsWith("lambda:") && lambda.Kids.Length == 1 ? lambda.Kids[0] : throw new UnsupportedException("a method group where a lambda goes");

    /// <summary>Two predicates on the same element as one: <c>x => a(x) &amp;&amp; b(x)</c>.</summary>
    static Node Conjoin(Node a, Node b)
    {
        var head = a.Head == "lambda:_" ? b.Head : a.Head;
        return Lambda(head, And(Body(Rebind(a, head)), Body(Rebind(b, head))));
    }

    static Node Negate(Node predicate) => Lambda(predicate.Head, Not(Body(predicate)));

    /// <summary><c>x => g(f(x))</c> for <c>Select(f)</c> then <c>g</c>.</summary>
    static Node Compose(Node f, Node g) => Lambda(f.Head, Substitute(Body(Rebind(g, f.Head)), Element(f.Head), Body(f)));

    public static Node Lambda(string head, Node body) => new(head, body);

    /// <summary>The element a lambda binds: <c>lambda:2</c> binds <c>elem:2</c>.</summary>
    public static Node Element(string lambdaHead) => new("elem:" + lambdaHead["lambda:".Length..]);

    /// <summary>The lambda made to bind the same element as <paramref name="head"/>.</summary>
    static Node Rebind(Node lambda, string head) =>
        lambda.Head == head ? lambda : Lambda(head, Substitute(Body(lambda), Element(lambda.Head), Element(head)));

    /// <summary><paramref name="n"/> with every <paramref name="what"/> replaced.</summary>
    public static Node Substitute(Node n, Node what, Node with)
    {
        if (n.Equals(what))
        {
            return with;
        }
        if (n.Kids.IsEmpty)
        {
            return n;
        }
        return Rebuild(n, n.Kids.Select(k => Substitute(k, what, with)).ToImmutableArray());
    }

    /// <summary>A node rebuilt with new children through the smart constructors.</summary>
    public static Node Rebuild(Node n, ImmutableArray<Node> kids)
    {
        if (kids.SequenceEqual(n.Kids))
        {
            return n;
        }
        if (n.Is("cond"))
        {
            return Choose(kids[0], kids[1], kids[2]);
        }
        if (n.Is("eq"))
        {
            return Equal("", kids[0], kids[1]);
        }
        if (n.Is("concat"))
        {
            return Concat(kids);
        }
        return n.StartsWith("lambda:") ? new Node(n.Head, kids) : Make(n.Head, kids);
    }

    // ---- facts --------------------------------------------------------------

    /// <summary>
    /// The tree simplified with what each branch knows: inside
    /// <c>c ? x : y</c>, <c>c</c> is true in <c>x</c> and false in <c>y</c>,
    /// and an element known to exist can be taken with <c>First</c>.
    /// </summary>
    public static Node Reduce(Node n, ImmutableDictionary<string, bool> facts)
    {
        if (n.Is("cond"))
        {
            return ReduceBranch(n, facts);
        }
        var kids = n.Kids.Select(k => Reduce(k, facts)).ToImmutableArray();
        if (n.Is("firstOrDefault") && Exists(n, facts))
        {
            Rule("FirstOrDefault where one exists → First");
            return new Node("first", kids);
        }
        return n.Kids.IsEmpty ? n : Rebuild(n, kids);
    }

    static Node ReduceBranch(Node n, ImmutableDictionary<string, bool> facts)
    {
        var test = n.Kids[0];
        if (facts.TryGetValue(test.Key, out var known))
        {
            return Reduce(known ? n.Kids[1] : n.Kids[2], facts);
        }
        var then = Reduce(n.Kids[1], facts.SetItem(test.Key, true));
        var otherwise = Reduce(n.Kids[2], facts.SetItem(test.Key, false));
        return Branch(Reduce(test, facts), then, otherwise);
    }

    /// <summary>The branch knows an element matching the search exists.</summary>
    static bool Exists(Node search, ImmutableDictionary<string, bool> facts) =>
        facts.TryGetValue(new Node("any", search.Kids).Key, out var exists) && exists;
}

public sealed class Trace
{
    public SortedSet<string> Rules { get; } = new(StringComparer.Ordinal);
    public SortedSet<string> Conditions { get; } = new(StringComparer.Ordinal);
}

static class Numbers
{
    static readonly HashSet<string> Integral = ["int", "long", "short", "byte", "uint", "ulong", "ushort", "sbyte", "char"];

    public static bool IsIntegral(string type) => Integral.Contains(type);

    /// <summary>Types where a &lt; b, a == b or a &gt; b always holds: not float or double.</summary>
    public static bool IsTotal(string type) =>
        IsIntegral(type) || type == "decimal" || type.StartsWith("enum", StringComparison.Ordinal) || Ordered.Contains(type);

    /// <summary>Framework values that are ordered like integers (ticks, days).</summary>
    static readonly HashSet<string> Ordered = ["System.DateTime", "System.DateOnly", "System.TimeOnly", "System.TimeSpan", "System.DateTimeOffset"];

    public static bool IsNumber(string type) => IsIntegral(type) || type is "decimal" or "double" or "float";

    /// <summary>The value of a numeric constant, or null.</summary>
    public static decimal? Value(Node n)
    {
        if (!n.StartsWith("const:"))
        {
            return null;
        }
        var parts = n.Head.Split(':', 3);
        return parts.Length == 3 && IsNumber(parts[1])
            && decimal.TryParse(parts[2], System.Globalization.NumberStyles.Float, System.Globalization.CultureInfo.InvariantCulture, out var value)
                ? value
                : null;
    }
}
