using System.Collections.Immutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Operations;

namespace DgfDup;

/// <summary>
/// One query on a table: a LINQ chain on <c>IQueryable&lt;T&gt;</c> (an EF
/// Core <c>DbSet&lt;T&gt;</c>, or a query of one), wherever it is in a method.
/// </summary>
/// <param name="Entity">The element type queried: the table.</param>
/// <param name="Filter">Every condition the query puts on <c>T</c>, as one
/// predicate: its <c>Where</c>s and the predicate of its last step joined.</param>
/// <param name="Shape">What it does besides filtering, in order: includes, ordering, projection.</param>
/// <param name="Terminal">How it ends: <c>FirstOrDefault</c>, <c>Any</c>, <c>ToList</c> (without <c>Async</c>), or empty.</param>
/// <param name="Holes">The values from outside its lambdas, in order: the parameters a shared version takes.</param>
/// <param name="Steps">Its filtering and shaping calls, outermost last.</param>
/// <param name="Root">The table or query it starts from.</param>
public sealed record QueryUse(
    INamedTypeSymbol Entity,
    Node Filter,
    ImmutableArray<Conjunct> Conjuncts,
    Node Shape,
    string Terminal,
    ImmutableArray<(SyntaxNode Syntax, ITypeSymbol Type)> Holes,
    ImmutableArray<IInvocationOperation> Steps,
    IOperation Root,
    string File,
    int StartLine,
    int EndLine,
    string Function,
    string Code,
    Trace Trace);

/// <summary>One condition of a query's filter (a part of a <c>&amp;&amp;</c>), with its code and the lambda parameter it reads.</summary>
public sealed record Conjunct(Node Atom, SyntaxNode Syntax, string Parameter);

public static class Queries
{
    static readonly HashSet<string> Owners =
    [
        "System.Linq.Queryable",
        "Microsoft.EntityFrameworkCore.EntityFrameworkQueryableExtensions",
        // ToList, ToArray and AsEnumerable on a query: where it runs.
        "System.Linq.Enumerable",
    ];

    /// <summary>Calls that end a query: they run it.</summary>
    static readonly HashSet<string> Terminals =
    [
        "First", "FirstOrDefault", "Single", "SingleOrDefault", "Last", "LastOrDefault", "Any", "All", "Count",
        "LongCount", "Sum", "Max", "Min", "Average", "ToList", "ToArray", "ToDictionary", "ToHashSet", "Contains",
        "ElementAt", "ElementAtOrDefault", "ExecuteDelete", "ExecuteUpdate", "Load", "AsEnumerable",
    ];

    /// <summary>Calls that change how a query runs, not what it returns.</summary>
    static readonly HashSet<string> Settings =
    [
        "AsNoTracking", "AsTracking", "AsNoTrackingWithIdentityResolution", "AsSplitQuery", "AsSingleQuery",
        "IgnoreQueryFilters", "IgnoreAutoIncludes", "TagWith", "TagWithCallSite",
    ];

    /// <summary>Every query in a method body: the outermost call of each chain.</summary>
    public static IEnumerable<QueryUse> In(IOperation body, string file, string function)
    {
        foreach (var call in body.DescendantsAndSelf().OfType<IInvocationOperation>())
        {
            if (IsStep(call) && !IsInnerStep(call) && Read(call, file, function) is { } use)
            {
                yield return use;
            }
        }
    }

    /// <summary>
    /// A query inside a lambda of another query (a correlated subquery, as
    /// in a <c>Select</c> projection): EF Core translates it into the outer
    /// query's SQL, and cannot translate a call to an extension method there.
    /// </summary>
    public static bool InsideAnotherQuery(QueryUse use)
    {
        var inLambda = false;
        for (var at = use.Root.Parent; at is not null; at = at.Parent)
        {
            inLambda |= at is IAnonymousFunctionOperation;
            if (inLambda && at is IInvocationOperation outer && IsStep(outer))
            {
                return true;
            }
        }
        return false;
    }

    static bool IsStep(IInvocationOperation call) =>
        call.TargetMethod.ContainingType?.ToDisplayString() is { } owner && Owners.Contains(owner)
        && call.Arguments.Length > 0 && ElementOf(Unwrapped(call.Arguments[0].Value).Type) is not null;

    /// <summary>A call whose result another step of the same chain takes.</summary>
    static bool IsInnerStep(IInvocationOperation call)
    {
        var parent = call.Parent;
        while (parent is IConversionOperation or IArgumentOperation)
        {
            parent = parent.Parent;
        }
        return parent is IInvocationOperation outer && IsStep(outer) && Unwrapped(outer.Arguments[0].Value) == call;
    }

    static IOperation Unwrapped(IOperation op) => op is IConversionOperation conversion ? Unwrapped(conversion.Operand) : op;

    /// <summary>The element type of <c>IQueryable&lt;T&gt;</c>, <c>DbSet&lt;T&gt;</c> and the like.</summary>
    static INamedTypeSymbol? ElementOf(ITypeSymbol? type)
    {
        if (type is not INamedTypeSymbol named)
        {
            return null;
        }
        var queryable = named.AllInterfaces.Append(named)
            .FirstOrDefault(i => i.OriginalDefinition.ToDisplayString() == "System.Linq.IQueryable<T>");
        return queryable?.TypeArguments[0] as INamedTypeSymbol;
    }

    static QueryUse? Read(IInvocationOperation outer, string file, string function)
    {
        var (steps, root) = Chain(outer);
        if (ElementOf(root.Type) is not { TypeKind: TypeKind.Class } entity)
        {
            return null;
        }
        var trace = Canon.Begin();
        try
        {
            return Lowered(entity, steps, root, file, function, trace);
        }
        catch (Exception why) when (Expected.Contains(why.GetType()))
        {
            return null;
        }
    }

    static readonly HashSet<Type> Expected = [typeof(UnsupportedException), typeof(InvalidCastException), typeof(InvalidOperationException)];

    /// <summary>The steps of a chain, innermost first, and what it starts from.</summary>
    static (List<IInvocationOperation> Steps, IOperation Root) Chain(IInvocationOperation outer)
    {
        var steps = new List<IInvocationOperation>();
        IOperation at = outer;
        while (at is IInvocationOperation call && IsStep(call))
        {
            steps.Insert(0, call);
            at = Unwrapped(call.Arguments[0].Value);
        }
        return (steps, at);
    }

    static QueryUse Lowered(INamedTypeSymbol entity, List<IInvocationOperation> steps, IOperation root, string file, string function, Trace trace)
    {
        var holes = new List<(SyntaxNode, ITypeSymbol)>();
        var own = OwnParameters(steps);
        var scope = new Scope { Hole = op => Hole(op, holes, own) };
        var last = steps[^1];
        var terminal = Terminals.Contains(Base(last.TargetMethod.Name)) ? Base(last.TargetMethod.Name) : "";
        var shaping = terminal.Length > 0 ? steps.Take(steps.Count - 1).ToList() : steps;
        var (conjuncts, shape, projected) = Shaped(shaping, scope);
        if (terminal.Length > 0 && !projected && FiltersAtTheEnd(last))
        {
            Canon.Rule("First(c) → Where(c).First()");
            conjuncts.AddRange(Conditions(last, scope));
        }
        // A query's conditions become SQL AND: their order does not matter.
        var filter = conjuncts.Select(c => c.Atom).Distinct().OrderBy(a => a.Key, StringComparer.Ordinal)
            .Aggregate(Node.True, Canon.And);
        var span = last.Syntax.GetLocation().GetLineSpan();
        return new QueryUse(entity, filter, [.. conjuncts], new Node("shape", shape.ToImmutableArray()), terminal, [.. holes],
            [.. shaping], root, file, span.StartLinePosition.Line + 1, span.EndLinePosition.Line + 1, function,
            last.Syntax.ToString(), trace);
    }

    /// <summary>The parameters of the lambdas in a chain's steps.</summary>
    static HashSet<IParameterSymbol> OwnParameters(List<IInvocationOperation> steps) =>
        steps.SelectMany(s => s.Arguments.Skip(1)).SelectMany(a => a.DescendantsAndSelf())
            .OfType<IAnonymousFunctionOperation>().SelectMany(l => l.Symbol.Parameters)
            .ToHashSet<IParameterSymbol>(SymbolEqualityComparer.Default);

    /// <summary>
    /// The filter (every Where while the query still holds the entity) and
    /// the other steps; and whether it was projected to something else.
    /// </summary>
    static (List<Conjunct> Conjuncts, List<Node> Shape, bool Projected) Shaped(List<IInvocationOperation> shaping, Scope scope)
    {
        var conjuncts = new List<Conjunct>();
        var shape = new List<Node>();
        var projected = false;
        foreach (var step in shaping.Where(s => !Settings.Contains(s.TargetMethod.Name)))
        {
            if (step.TargetMethod.Name == "Where" && !projected)
            {
                conjuncts.AddRange(Conditions(step, scope));
                continue;
            }
            projected |= step.TargetMethod.Name is "Select" or "SelectMany" or "GroupBy";
            shape.Add(new Node("step:" + step.TargetMethod.Name, step.Arguments.Skip(1).Select(a => Lower.Expr(a.Value, scope)).ToImmutableArray()));
        }
        return (conjuncts, shape, projected);
    }

    /// <summary>A last step with a predicate: <c>FirstOrDefault(x => ...)</c>.</summary>
    static bool FiltersAtTheEnd(IInvocationOperation last) =>
        last.Arguments.Length == 2 && IsPredicate(last.Arguments[1].Value);

    static string Base(string name) => name.EndsWith("Async", StringComparison.Ordinal) ? name[..^5] : name;

    static bool IsPredicate(IOperation arg) =>
        Unwrapped(arg) is IAnonymousFunctionOperation or IDelegateCreationOperation;

    /// <summary>The conditions of a step's predicate: each part of its top-level <c>&amp;&amp;</c>, its parameter as element 0.</summary>
    static IEnumerable<Conjunct> Conditions(IInvocationOperation step, Scope scope)
    {
        if (Lambda(step.Arguments[1].Value) is not { Symbol.Parameters: [var parameter] } lambda
            || lambda.Body.Operations is not [IReturnOperation { ReturnedValue: { } body }])
        {
            throw new UnsupportedException("a predicate that is not a one-parameter expression lambda");
        }
        var inner = scope.Set(parameter, new Node("elem:0")) with { Depth = 1 };
        return Parts(body).Select(part => new Conjunct(Canon.Atom(Lower.Expr(part, inner)), part.Syntax, parameter.Name)).ToList();
    }

    static IAnonymousFunctionOperation? Lambda(IOperation op) => Unwrapped(op) switch
    {
        IAnonymousFunctionOperation lambda => lambda,
        IDelegateCreationOperation { Target: var target } => Lambda(target),
        _ => null,
    };

    /// <summary>The parts of <c>a &amp;&amp; b &amp;&amp; c</c>, in order.</summary>
    static IEnumerable<IOperation> Parts(IOperation op) =>
        Unwrapped(op) is IBinaryOperation { OperatorKind: BinaryOperatorKind.ConditionalAnd, OperatorMethod: null } and
            ? Parts(and.LeftOperand).Concat(Parts(and.RightOperand))
            : [op];

    /// <summary>
    /// A value from outside every lambda (it reads no lambda's parameter)
    /// and not a constant: it becomes a hole, recorded with its syntax.
    /// </summary>
    static Node? Hole(IOperation op, List<(SyntaxNode, ITypeSymbol)> holes, HashSet<IParameterSymbol> own)
    {
        if (op.Type is null || op.ConstantValue.HasValue || op is IArgumentOperation || HasLambda(op, own))
        {
            return null;
        }
        var at = holes.FindIndex(h => h.Item1.ToString() == op.Syntax.ToString());
        if (at < 0)
        {
            holes.Add((op.Syntax, op.Type));
            at = holes.Count - 1;
        }
        return new Node($"hole:{at}:{Types.Name(op.Type)}");
    }

    /// <summary>
    /// A lambda, or a read of a parameter of the query's own lambdas, anywhere
    /// in it. An outer lambda's parameter (a subquery's <c>r.MemberId</c>)
    /// is a value from outside, like a local.
    /// </summary>
    static bool HasLambda(IOperation op, HashSet<IParameterSymbol> own) =>
        op.DescendantsAndSelf().Any(d => d is IAnonymousFunctionOperation || d is IParameterReferenceOperation p && own.Contains(p.Parameter));
}
