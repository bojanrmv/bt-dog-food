using System.Collections.Immutable;
using Microsoft.CodeAnalysis;

namespace DgfDup;

/// <summary>
/// What an expression means where it is: the values locals hold, which
/// element a loop or lambda variable stands for, how deep in lambdas it is,
/// and what the branches taken so far know.
/// </summary>
public sealed record Scope
{
    public ImmutableDictionary<ISymbol, Node> Values { get; init; } =
        ImmutableDictionary.Create<ISymbol, Node>(SymbolEqualityComparer.Default);

    /// <summary>Lambdas and loops around: the next one binds <c>elem:Depth</c>.</summary>
    public int Depth { get; init; }

    /// <summary>Tests decided on the way here, by key.</summary>
    public ImmutableDictionary<string, bool> Facts { get; init; } = ImmutableDictionary<string, bool>.Empty;

    /// <summary>In an index loop: the index, the collection it walks and the element <c>xs[i]</c> is.</summary>
    public (ILocalSymbol Index, Node Source, Node Element)? Index { get; init; }

    /// <summary>What <c>a?.</c> reads: the value of <c>a</c>.</summary>
    public Node? Accessed { get; init; }

    /// <summary>Inside a loop body: locals from outside that it changes, by their placeholders.</summary>
    public ImmutableArray<ILocalSymbol> Accumulators { get; init; } = ImmutableArray<ILocalSymbol>.Empty;

    /// <summary>Running a loop body: its end, <c>continue</c>, <c>break</c> and <c>return</c> are leaves.</summary>
    public bool InLoop { get; init; }

    /// <summary>The method's parameters, for numbering them.</summary>
    public ImmutableArray<IParameterSymbol> Parameters { get; init; } = ImmutableArray<IParameterSymbol>.Empty;

    /// <summary>
    /// Values with effects bound on this path, in order: a local holding one
    /// (an await, a call, a new object) is not copied to where it is used, so
    /// one call stays one call.
    /// </summary>
    public ImmutableList<Node> Lets { get; init; } = ImmutableList<Node>.Empty;

    /// <summary>Statements run so far on all paths: a guard against paths multiplying.</summary>
    public Budget Steps { get; init; } = new();

    public Scope Set(ISymbol symbol, Node value) => this with { Values = Values.SetItem(symbol, value) };

    /// <summary>A local's value: inlined when pure, else bound once and referred to.</summary>
    public Scope Assign(ISymbol symbol, Node value)
    {
        if (Purity.IsPure(value))
        {
            return Set(symbol, value);
        }
        if (InLoop)
        {
            throw new UnsupportedException("a loop body that keeps a value with effects");
        }
        var reference = new Node($"ref:{Lets.Count}");
        return this with { Lets = Lets.Add(value), Values = Values.SetItem(symbol, reference) };
    }

    /// <summary>A path's result with the values it bound: <c>let(v0, v1, ..., result)</c>.</summary>
    public Node Result(Node value) => Lets.IsEmpty ? value : new Node("let", Lets.Add(value).ToImmutableArray());

    public Scope Know(Node test, bool value) => this with { Facts = Facts.SetItem(test.Key, value) };

    public Node Get(ISymbol symbol) =>
        Values.TryGetValue(symbol, out var value) ? value : throw new UnsupportedException($"reads {symbol.Name} before it has a value");
}

public sealed class Budget
{
    const int Most = 2000;
    int used;

    public void Spend()
    {
        if (++used > Most)
        {
            throw new UnsupportedException("too many paths");
        }
    }
}
