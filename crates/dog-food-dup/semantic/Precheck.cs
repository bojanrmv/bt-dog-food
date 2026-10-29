using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace DgfDup;

/// <summary>
/// What a method's syntax alone shows the engine would leave it out for:
/// checked before binding and running it, as most methods are left out and
/// many only at their last statement, after every path to it has run.
/// Each answer is one <see cref="Exec"/> gives too, on any path that
/// reaches the statement.
/// </summary>
public static class Precheck
{
    /// <summary>Why the method is left out, or null when it may reduce.</summary>
    public static string? Why(MethodDeclarationSyntax method)
    {
        if (ReturnsNothing(method.ReturnType))
        {
            return "returns no value";
        }
        if (method.Body is not { } body)
        {
            return null;
        }
        foreach (var statement in Statements(body))
        {
            if (Unmodelled(statement) is { } why)
            {
                return why;
            }
        }
        return null;
    }

    /// <summary><c>void</c>, <c>Task</c> and <c>ValueTask</c>: no path returns a value.</summary>
    static bool ReturnsNothing(TypeSyntax type) =>
        type is PredefinedTypeSyntax predefined
            ? predefined.Keyword.IsKind(SyntaxKind.VoidKeyword)
            : LastName(type) is "Task" or "ValueTask";

    /// <summary><c>Task</c> of <c>System.Threading.Tasks.Task</c>; null for a generic or other type.</summary>
    static string? LastName(TypeSyntax type) =>
        ((type as QualifiedNameSyntax)?.Right ?? type) is IdentifierNameSyntax name ? name.Identifier.Text : null;

    /// <summary>The body's statements, not those of the lambdas and local functions in it.</summary>
    static IEnumerable<StatementSyntax> Statements(BlockSyntax body) =>
        body.DescendantNodes(n => n is not (AnonymousFunctionExpressionSyntax or LocalFunctionStatementSyntax))
            .OfType<StatementSyntax>();

    static string? Unmodelled(StatementSyntax statement) => statement switch
    {
        LocalDeclarationStatementSyntax { UsingKeyword.RawKind: not 0 } => "a UsingDeclaration statement",
        ExpressionStatementSyntax expression => Effect(expression.Expression),
        _ => Unrun.GetValueOrDefault(statement.GetType()),
    };

    /// <summary>Statements the engine does not run, by their syntax.</summary>
    static readonly Dictionary<Type, string> Unrun = new()
    {
        [typeof(TryStatementSyntax)] = "a Try statement",
        [typeof(UsingStatementSyntax)] = "a Using statement",
        [typeof(LockStatementSyntax)] = "a Lock statement",
        [typeof(YieldStatementSyntax)] = "a yield statement",
        [typeof(SwitchStatementSyntax)] = "a Switch statement",
        [typeof(GotoStatementSyntax)] = "a goto",
        [typeof(LabeledStatementSyntax)] = "a goto",
        [typeof(LocalFunctionStatementSyntax)] = "a LocalFunction statement",
        [typeof(CheckedStatementSyntax)] = "a checked block",
        [typeof(UnsafeStatementSyntax)] = "an unsafe block",
        [typeof(FixedStatementSyntax)] = "a fixed block",
        [typeof(DoStatementSyntax)] = "a do loop",
        [typeof(ForEachVariableStatementSyntax)] = "foreach with a deconstruction",
    };

    /// <summary>
    /// A statement run for its effect: only a local set or changed, an
    /// indexer set, or a call chain on a local is modelled.
    /// </summary>
    static string? Effect(ExpressionSyntax expression) => expression switch
    {
        AssignmentExpressionSyntax set => Assigned(set.Left),
        InvocationExpressionSyntax call => OnName(call) ? null : "a call for its effect",
        _ => Effects.GetValueOrDefault(expression.GetType()) ?? Stepped(expression),
    };

    static readonly Dictionary<Type, string> Effects = new()
    {
        [typeof(AwaitExpressionSyntax)] = "Await as a statement",
        [typeof(ConditionalAccessExpressionSyntax)] = "a ?. call as a statement",
    };

    /// <summary>A member set, or a deconstruction: only locals and indexers are set.</summary>
    static string? Assigned(ExpressionSyntax left) => left switch
    {
        MemberAccessExpressionSyntax or MemberBindingExpressionSyntax => "a member set as a statement",
        TupleExpressionSyntax or DeclarationExpressionSyntax => "a deconstruction",
        _ => null,
    };

    /// <summary><c>x.Count++</c>: only a local is changed.</summary>
    static string? Stepped(ExpressionSyntax step) =>
        step is PostfixUnaryExpressionSyntax or PrefixUnaryExpressionSyntax
        && step.ChildNodes().OfType<ExpressionSyntax>().FirstOrDefault() is MemberAccessExpressionSyntax
            ? "a member changed as a statement"
            : null;

    /// <summary><c>sb.Append(a).Append(b)</c>: a chain of calls on a name (which may be a local).</summary>
    static bool OnName(InvocationExpressionSyntax call)
    {
        ExpressionSyntax at = call;
        while (at is InvocationExpressionSyntax { Expression: MemberAccessExpressionSyntax member })
        {
            at = member.Expression;
        }
        return at is IdentifierNameSyntax;
    }
}
