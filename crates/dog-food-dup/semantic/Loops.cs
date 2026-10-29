using System.Collections.Immutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Operations;

namespace DgfDup;

/// <summary>
/// Loops as what they compute. A <c>foreach</c>, an index <c>for</c> and a
/// <c>while</c> over <c>i &lt; xs.Count</c> all walk the elements of
/// <c>xs</c>. The body runs once, symbolically, with each local it changes
/// as a placeholder; how each body ends says which loop it is:
/// <list type="bullet">
/// <item>a fold: every end changes the locals, by the same step under some
/// condition (<c>sum += f(x)</c>, <c>count++</c>, <c>if (f(x) &gt; max) max = f(x)</c>,
/// <c>list.Add(f(x))</c>, a sum per key);</item>
/// <item>a search: some ends <c>return v</c>, the others go on unchanged: the
/// first element that returns decides (<c>Any</c>, <c>First</c>);</item>
/// <item>a flag: some ends set locals and <c>break</c>: the first such
/// element decides their values.</item>
/// </list>
/// </summary>
public static class Loops
{
    public static Node Run(ILoopOperation loop, ImmutableList<IOperation> rest, Scope s)
    {
        var (source, body, bind, after) = Walk(loop, s);
        var element = new Node($"elem:{s.Depth}");
        var accumulators = Changed(loop.Body).ToImmutableArray();
        var inner = bind(s, element) with
        {
            Depth = s.Depth + 1,
            InLoop = true,
            Accumulators = accumulators,
        };
        for (var k = 0; k < accumulators.Length; k++)
        {
            inner = inner.Set(accumulators[k], Placeholder(s, accumulators[k], k));
        }
        var ends = Exec.Run(body, inner);
        var lambda = $"lambda:{s.Depth}";
        var loopEnds = new Ends(ends, source, element, lambda, accumulators, s);
        return loopEnds.Kind switch
        {
            "ret" => Search(loopEnds, rest, after(s)),
            "break" => Exec.Run(rest, Flag(loopEnds, after(s))),
            _ => Exec.Run(rest, Fold(loopEnds, after(s))),
        };
    }

    static Node Placeholder(Scope s, ILocalSymbol local, int k)
    {
        var kind = s.Values.TryGetValue(local, out var value) ? Calls.Kind(value) ?? "value" : "value";
        return new Node($"acc:{kind}:{k}");
    }

    /// <summary>The collection, the body's statements, how the element is bound, and the scope after the loop.</summary>
    static (Node Source, ImmutableList<IOperation> Body, Func<Scope, Node, Scope> Bind, Func<Scope, Scope> After) Walk(ILoopOperation loop, Scope s) => loop switch
    {
        IForEachLoopOperation each => ForEach(each, s),
        IForLoopOperation indexed => For(indexed, s),
        IWhileLoopOperation { ConditionIsTop: true, ConditionIsUntil: false } counted => While(counted, s),
        _ => throw new UnsupportedException($"a {loop.LoopKind} loop"),
    };

    static (Node, ImmutableList<IOperation>, Func<Scope, Node, Scope>, Func<Scope, Scope>) ForEach(IForEachLoopOperation loop, Scope s)
    {
        if (loop.LoopControlVariable is not IVariableDeclaratorOperation variable)
        {
            throw new UnsupportedException("foreach with a deconstruction");
        }
        var source = Lower.Expr(loop.Collection, s);
        return (source, Statements(loop.Body), (scope, element) => scope.Set(variable.Symbol, element), scope => scope);
    }

    /// <summary><c>for (var i = 0; i &lt; xs.Count; i++)</c>: the elements of <c>xs</c>, with <c>xs[i]</c> the element.</summary>
    static (Node, ImmutableList<IOperation>, Func<Scope, Node, Scope>, Func<Scope, Scope>) For(IForLoopOperation loop, Scope s)
    {
        if (ZeroIndex(loop) is not { } index || !StepsByOne(loop, index) || loop.Condition is null)
        {
            throw new UnsupportedException("a for loop that does not count from 0 by one");
        }
        var source = Bound(loop.Condition, index, s);
        Canon.Rule("index loop → each element");
        return (source, Statements(loop.Body), (scope, element) => scope with { Index = (index, source, element) }, scope => scope);
    }

    /// <summary>The index a for loop declares, when it starts at 0.</summary>
    static ILocalSymbol? ZeroIndex(IForLoopOperation loop) =>
        loop.Before is [IVariableDeclarationGroupOperation group]
        && group.Declarations.SelectMany(d => d.Declarators).SingleOrDefault() is { } declarator
        && declarator.Initializer?.Value.ConstantValue is { HasValue: true, Value: 0 }
            ? declarator.Symbol
            : null;

    static bool StepsByOne(IForLoopOperation loop, ILocalSymbol index) =>
        loop.AtLoopBottom is [IExpressionStatementOperation { Operation: IIncrementOrDecrementOperation { Kind: OperationKind.Increment, Target: ILocalReferenceOperation step } }]
        && SymbolEqualityComparer.Default.Equals(step.Local, index);

    /// <summary><c>var i = 0; while (i &lt; xs.Count) { ...; i++; }</c>, as an index loop.</summary>
    static (Node, ImmutableList<IOperation>, Func<Scope, Node, Scope>, Func<Scope, Scope>) While(IWhileLoopOperation loop, Scope s)
    {
        var body = Statements(loop.Body);
        if (body.IsEmpty || body[^1] is not IExpressionStatementOperation { Operation: IIncrementOrDecrementOperation { Kind: OperationKind.Increment, Target: ILocalReferenceOperation step } }
            || loop.Condition is null
            || !s.Values.TryGetValue(step.Local, out var start) || !start.Equals(Node.Const("int", "0")))
        {
            throw new UnsupportedException("a while loop that is not an index loop");
        }
        var index = step.Local;
        var source = Bound(loop.Condition, index, s);
        Canon.Rule("index loop → each element");
        return (source, body.RemoveAt(body.Count - 1),
            (scope, element) => scope with { Index = (index, source, element), Values = scope.Values.Remove(index) },
            scope => scope.Set(index, Canon.Count(source, null)));
    }

    /// <summary>The collection in <c>i &lt; xs.Count</c>.</summary>
    static Node Bound(IOperation condition, ILocalSymbol index, Scope s)
    {
        if (condition is IBinaryOperation { OperatorKind: BinaryOperatorKind.LessThan, LeftOperand: ILocalReferenceOperation left } less
            && SymbolEqualityComparer.Default.Equals(left.Local, index)
            && Lower.Expr(less.RightOperand, s) is { } count && count.Is("count") && count.Kids.Length == 1)
        {
            return count.Kids[0];
        }
        throw new UnsupportedException("an index loop that does not run to the end of a collection");
    }

    static ImmutableList<IOperation> Statements(IOperation body) =>
        body is IBlockOperation block ? block.Operations.ToImmutableList() : ImmutableList.Create(body);

    /// <summary>Locals from outside the body that it changes.</summary>
    static IEnumerable<ILocalSymbol> Changed(IOperation body)
    {
        var inside = body.Descendants().OfType<IVariableDeclaratorOperation>().Select(d => d.Symbol)
            .ToHashSet<ILocalSymbol>(SymbolEqualityComparer.Default);
        var changed = new List<ILocalSymbol>();
        foreach (var op in body.DescendantsAndSelf())
        {
            var local = Target(op);
            if (local is not null && !inside.Contains(local) && !changed.Contains(local, SymbolEqualityComparer.Default))
            {
                changed.Add(local);
            }
        }
        return changed;
    }

    static ILocalSymbol? Target(IOperation op) => op switch
    {
        IAssignmentOperation { Target: ILocalReferenceOperation local } => local.Local,
        IAssignmentOperation { Target: IPropertyReferenceOperation { Instance: ILocalReferenceOperation local } } => local.Local,
        IIncrementOrDecrementOperation { Target: ILocalReferenceOperation local } => local.Local,
        IInvocationOperation { Instance: ILocalReferenceOperation local, TargetMethod.Name: "Add" or "Append" } => local.Local,
        _ => null,
    };

    // ---- search and flag loops ----------------------------------------------

    /// <summary>A loop that returns on the first element that matches: <c>Any(c) ? v(First(c)) : rest</c>.</summary>
    static Node Search(Ends ends, ImmutableList<IOperation> rest, Scope s)
    {
        ends.RequireUnchanged("next");
        var value = ends.Single("ret", 0);
        var predicate = ends.Predicate("ret");
        Canon.Rule("loop that returns on a match → Any / First");
        var found = Canon.Substitute(value, ends.Element, new Node("first", ends.Source, predicate));
        var any = Canon.Any(ends.Source, predicate);
        // Inside another loop's body, the return is that body's end.
        var result = s.InLoop ? new Node("ret", found) : found;
        return Exec.Fork(any, s, inner => result, inner => Exec.Run(rest, inner));
    }

    /// <summary>A loop that sets locals and breaks on the first element that matches.</summary>
    static Scope Flag(Ends ends, Scope s)
    {
        ends.RequireUnchanged("next");
        var predicate = ends.Predicate("break");
        var any = Canon.Any(ends.Source, predicate);
        Canon.Rule("loop that breaks on a match → Any / First");
        for (var k = 0; k < ends.Accumulators.Length; k++)
        {
            var value = ends.Single("break", k);
            if (value.Equals(ends.Placeholder(k)))
            {
                continue;
            }
            var found = Canon.Substitute(value, ends.Element, new Node("first", ends.Source, predicate));
            s = s.Set(ends.Accumulators[k], Canon.Choose(any, found, s.Get(ends.Accumulators[k])));
        }
        return s;
    }

    // ---- folds -----------------------------------------------------------------

    /// <summary>A loop that changes locals at every element: each local's fold.</summary>
    static Scope Fold(Ends ends, Scope s)
    {
        for (var k = 0; k < ends.Accumulators.Length; k++)
        {
            var local = ends.Accumulators[k];
            var step = ends.Step(k);
            if (!step.Equals(ends.Placeholder(k)))
            {
                s = s.Set(local, Folds.Of(step, ends.Placeholder(k), s.Get(local), ends, s));
            }
        }
        return s;
    }
}

/// <summary>How a loop body ends: a decision tree whose leaves are <c>next</c>, <c>break</c> and <c>ret</c>.</summary>
public sealed class Ends(Node tree, Node source, Node element, string lambda, ImmutableArray<ILocalSymbol> accumulators, Scope outer)
{
    public Node Source { get; } = source;
    public Node Element { get; } = element;
    public string Lambda { get; } = lambda;
    public ImmutableArray<ILocalSymbol> Accumulators { get; } = accumulators;
    public Scope Outer { get; } = outer;

    public Node Placeholder(int k) => new($"acc:{KindOf(k)}:{k}");

    string KindOf(int k) => Outer.Values.TryGetValue(Accumulators[k], out var value) ? Calls.Kind(value) ?? "value" : "value";

    /// <summary><c>ret</c> when some end returns, <c>break</c> when some breaks, else <c>next</c>.</summary>
    public string Kind => Leaves().Any(l => l.Is("ret")) ? "ret" : Leaves().Any(l => l.Is("break")) ? "break" : "next";

    IEnumerable<Node> Leaves() => LeavesOf(tree);

    static IEnumerable<Node> LeavesOf(Node n) => n.Is("cond") ? LeavesOf(n.Kids[1]).Concat(LeavesOf(n.Kids[2])) : [n];

    /// <summary>Every end of this kind leaves the locals as they were.</summary>
    public void RequireUnchanged(string kind)
    {
        foreach (var leaf in Leaves().Where(l => l.Is(kind)))
        {
            for (var k = 0; k < leaf.Kids.Length; k++)
            {
                if (!leaf.Kids[k].Equals(Placeholder(k)))
                {
                    throw new UnsupportedException("a loop that both changes locals and stops early");
                }
            }
        }
        if (Leaves().Any(l => !l.Is(kind) && !l.Is(Kind)))
        {
            throw new UnsupportedException("a loop with both break and return");
        }
    }

    /// <summary>The one value the ends of this kind have at position <paramref name="k"/>.</summary>
    public Node Single(string kind, int k)
    {
        var values = Leaves().Where(l => l.Is(kind)).Select(l => l.Kids[k]).Distinct().ToList();
        return values.Count == 1 ? values[0] : throw new UnsupportedException("a loop whose matches give different values");
    }

    /// <summary>The test for an element ending in <paramref name="kind"/>, as a lambda.</summary>
    public Node Predicate(string kind) => Canon.Lambda(Lambda, Map(tree, leaf => leaf.Is(kind) ? Node.True : Node.False));

    /// <summary>What local <paramref name="k"/> holds at the end of the body: a tree of its new values.</summary>
    public Node Step(int k) => Map(tree, leaf => leaf.Kids[k]);

    static Node Map(Node n, Func<Node, Node> leaf) =>
        n.Is("cond") ? Canon.Choose(Canon.Atom(n.Kids[0]), Map(n.Kids[1], leaf), Map(n.Kids[2], leaf)) : leaf(n);
}
