namespace DgfDup;

/// <summary>
/// What a fold leaves in a local: the body changes it the same way at every
/// element that passes a guard, and leaves it alone at the others.
/// <list type="bullet">
/// <item><c>acc + f(x)</c>: <c>old + Sum(Where(guard), f)</c>; <c>acc + 1</c>: <c>old + Count(Where(guard))</c>;</item>
/// <item><c>max(acc, f(x))</c>, started from the first element where the
/// collection is known not to be empty: <c>Max(f)</c>;</item>
/// <item><c>list.Add(f(x))</c> on an empty list: <c>Where(guard).Select(f).ToList()</c>;</item>
/// <item><c>d[k] = (d has k ? d[k] : 0) + v</c> on an empty dictionary: a sum per key.</item>
/// </list>
/// </summary>
public static class Folds
{
    /// <summary>One local's fold: its step, its placeholder, its value before the loop and the loop.</summary>
    sealed record Fold(Node Update, Node Acc, Node Old, Node Guard, Ends Ends, Scope Scope)
    {
        public Node Source => Canon.Where(Ends.Source, Guard);

        public string Type => Update.Head[(Update.Head.LastIndexOf(':') + 1)..];

        public Node Lambda(Node body) => Canon.Lambda(Ends.Lambda, body);

        /// <summary>The operand of a two-operand step that is not the running value, when the other one is.</summary>
        public Node? Operand()
        {
            if (Update.Kids.Length != 2)
            {
                return null;
            }
            var other = Update.Kids[0].Equals(Acc) ? Update.Kids[1] : Update.Kids[1].Equals(Acc) ? Update.Kids[0] : null;
            return other is null || Uses(other, Acc) ? null : other;
        }
    }

    public static Node Of(Node step, Node acc, Node old, Ends ends, Scope s)
    {
        if (PerKey(step, acc, old, ends) is { } perKey)
        {
            return perKey;
        }
        var updates = Leaves(step).Where(l => !l.Equals(acc)).Distinct().ToList();
        if (updates.Count != 1)
        {
            throw new UnsupportedException("a loop that changes a local in different ways");
        }
        var guard = Canon.Lambda(ends.Lambda, Map(step, leaf => leaf.Equals(acc) ? Node.False : Node.True));
        if (Uses(guard, acc))
        {
            throw new UnsupportedException("a loop whose test reads the value it builds");
        }
        var fold = new Fold(updates[0], acc, old, guard, ends, s);
        return Rules.Select(rule => rule(fold)).FirstOrDefault(n => n is not null)
            ?? throw new UnsupportedException($"a loop that builds {fold.Update.Head}");
    }

    static readonly Func<Fold, Node?>[] Rules = [Total, Extreme, Listed];

    /// <summary><c>acc + f(x)</c> or <c>acc - f(x)</c> at each element.</summary>
    static Node? Total(Fold fold)
    {
        if (!Adds(fold) || fold.Operand() is not { } f)
        {
            return null;
        }
        // op:+:T or op:-:T, the same operator around the total.
        var head = fold.Update.Head;
        if (IsOne(f, fold.Type))
        {
            Canon.Rule("loop that counts → Count");
            Canon.Assume("the count does not overflow " + fold.Type);
            return Canon.Make(head, fold.Old, Canon.Count(fold.Source, null));
        }
        Canon.Rule("loop that adds up → Sum");
        return Canon.Make(head, fold.Old, Canon.Sum(fold.Type, fold.Source, fold.Lambda(f)));
    }

    /// <summary>A step that adds to the running value, or takes from it.</summary>
    static bool Adds(Fold fold) =>
        fold.Update.StartsWith("op:+:") || fold.Update.StartsWith("op:-:") && fold.Update.Kids[0].Equals(fold.Acc);

    static bool IsOne(Node f, string type) => f.Equals(Node.Const(type, "1")) && Numbers.IsIntegral(type);

    /// <summary><c>max = xs[0].f; foreach x: if (x.f &gt; max) max = x.f</c> where <c>xs</c> is not empty: <c>Max(f)</c>.</summary>
    static Node? Extreme(Fold fold)
    {
        if (!IsExtreme(fold) || fold.Operand() is not { } f || !StartsAtFirst(fold, f) || !KnownNotEmpty(fold))
        {
            return null;
        }
        Canon.Rule("loop that keeps the largest → Max");
        return fold.Update.StartsWith("max:")
            ? Canon.Max(fold.Type, fold.Ends.Source, fold.Lambda(f))
            : new Node($"minOf:{fold.Type}", fold.Ends.Source, fold.Lambda(f));
    }

    /// <summary>A step that keeps the larger (or smaller) value, at every element.</summary>
    static bool IsExtreme(Fold fold) =>
        (fold.Update.StartsWith("max:") || fold.Update.StartsWith("min:")) && Canon.Body(fold.Guard).Equals(Node.True);

    /// <summary>The value before the loop is <c>f</c> of the collection's first element.</summary>
    static bool StartsAtFirst(Fold fold, Node f)
    {
        var first = Find(fold.Old, n => n.StartsWith("index:") && n.Kids.Length == 2 && n.Kids[0].Equals(fold.Ends.Source) && n.Kids[1].Equals(Node.Const("int", "0")));
        return first is not null && Canon.Substitute(fold.Old, first, fold.Ends.Element).Equals(f);
    }

    /// <summary>
    /// The path to the loop knows the collection is not empty: empty, the
    /// loop's start would throw where <c>Max</c> throws another exception.
    /// </summary>
    static bool KnownNotEmpty(Fold fold) =>
        fold.Scope.Facts.TryGetValue(Canon.NotEmpty(fold.Ends.Source).Kids[0].Key, out var known) && known;

    /// <summary><c>list.Add(f(x))</c> on an empty list.</summary>
    static Node? Listed(Fold fold)
    {
        var appends = fold.Update.Is("append") && fold.Update.Kids[0].Equals(fold.Acc) && !Uses(fold.Update.Kids[1], fold.Acc);
        if (!appends || !fold.Old.Equals(new Node("list")))
        {
            return null;
        }
        Canon.Rule("loop that adds to a list → Where / Select / ToList");
        var f = fold.Update.Kids[1];
        var selected = f.Equals(fold.Ends.Element) ? fold.Source : new Node("select", fold.Source, fold.Lambda(f));
        return new Node("toList", selected);
    }

    /// <summary>
    /// <c>d[k] = d.TryGetValue(k, out c) ? c + v : v</c> (or with
    /// <c>GetValueOrDefault</c>) on an empty dictionary: a sum per key, as
    /// <c>GroupBy(k).ToDictionary(g =&gt; g.Key, g =&gt; g.Sum(v))</c> makes.
    /// </summary>
    static Node? PerKey(Node step, Node acc, Node old, Ends ends)
    {
        if (!old.Is("dict") || KeyTested(step, acc) is not { } key)
        {
            return null;
        }
        var stored = SetValue(step.Kids[2], acc, key);
        var added = SetValue(step.Kids[1], acc, key);
        if (stored is null || added is null || !SumsPerKey(added, stored, acc, key))
        {
            return null;
        }
        Canon.Rule("loop that adds up per key → GroupBy / Sum");
        var type = added.Head[(added.Head.LastIndexOf(':') + 1)..];
        return new Node("groupSum:" + type, ends.Source, Canon.Lambda(ends.Lambda, key), Canon.Lambda(ends.Lambda, stored));
    }

    /// <summary>The key of a step that branches on "the running dictionary has the key".</summary>
    static Node? KeyTested(Node step, Node acc) =>
        step.Is("cond") && step.Kids[0].Is("has") && step.Kids[0].Kids[0].Equals(acc) ? step.Kids[0].Kids[1] : null;

    /// <summary>The value of <c>d[key] = value</c> on the running dictionary.</summary>
    static Node? SetValue(Node n, Node acc, Node key) =>
        n.Is("set") && n.Kids[0].Equals(acc) && n.Kids[1].Equals(key) ? n.Kids[2] : null;

    /// <summary>Where the key is there, the value is added to it; where not, the value is stored.</summary>
    static bool SumsPerKey(Node added, Node stored, Node acc, Node key) =>
        !Uses(stored, acc) && !Uses(key, acc) && AddsTo(added, Calls.Get(acc, key), stored);

    /// <summary><paramref name="sum"/> is <c>current + value</c>.</summary>
    static bool AddsTo(Node sum, Node current, Node value) =>
        sum.StartsWith("op:+:") && sum.Kids.Contains(current) && sum.Kids.Contains(value);

    static IEnumerable<Node> Leaves(Node n) => n.Is("cond") ? Leaves(n.Kids[1]).Concat(Leaves(n.Kids[2])) : [n];

    static Node Map(Node n, Func<Node, Node> leaf) =>
        n.Is("cond") ? Canon.Choose(Canon.Atom(n.Kids[0]), Map(n.Kids[1], leaf), Map(n.Kids[2], leaf)) : leaf(n);

    static bool Uses(Node n, Node part) => n.Equals(part) || n.Kids.Any(k => Uses(k, part));

    static Node? Find(Node n, Func<Node, bool> match) =>
        match(n) ? n : n.Kids.Select(k => Find(k, match)).FirstOrDefault(found => found is not null);
}
