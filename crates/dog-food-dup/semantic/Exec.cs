using System.Collections.Immutable;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Operations;

namespace DgfDup;

/// <summary>
/// A method body run symbolically, to the value it returns. Locals hold
/// expressions, so they disappear (inlined); an <c>if</c> splits the run,
/// each branch going on with the rest of the body and knowing the test's
/// outcome, so early returns, nested ifs and <c>?:</c> end as the same
/// decision tree. In a loop body the ends (<c>continue</c>, the body's end,
/// <c>break</c>, <c>return</c>) are leaves the loop rules read.
/// </summary>
public static class Exec
{
    public static Node Body(IBlockOperation body, Scope s) => Run(body.Operations.ToImmutableList(), s);

    public static Node Run(ImmutableList<IOperation> statements, Scope s)
    {
        s.Steps.Spend();
        if (statements.IsEmpty)
        {
            return s.InLoop ? Leaf("next", s) : throw new UnsupportedException("a path that returns nothing");
        }
        var head = statements[0];
        return Statements.TryGetValue(head.Kind, out var run)
            ? run(head, statements.RemoveAt(0), s)
            : throw new UnsupportedException($"a {head.Kind} statement");
    }

    /// <summary>One handler per kind of statement: given the statement, the rest of the body and the scope.</summary>
    static readonly Dictionary<OperationKind, Func<IOperation, ImmutableList<IOperation>, Scope, Node>> Statements = new()
    {
        [OperationKind.Block] = (op, rest, s) => Run(((IBlockOperation)op).Operations.ToImmutableList().AddRange(rest), s),
        [OperationKind.VariableDeclarationGroup] = (op, rest, s) => Run(rest, Declare((IVariableDeclarationGroupOperation)op, s)),
        [OperationKind.ExpressionStatement] = (op, rest, s) => Expression(((IExpressionStatementOperation)op).Operation, rest, s),
        [OperationKind.Throw] = (op, rest, s) => s.Result(Lower.Expr(op, s)),
        [OperationKind.Return] = (op, rest, s) => Return((IReturnOperation)op, s),
        [OperationKind.Conditional] = (op, rest, s) => If((IConditionalOperation)op, rest, s),
        [OperationKind.Branch] = (op, rest, s) => Branch((IBranchOperation)op, s),
        [OperationKind.Loop] = (op, rest, s) => Loops.Run((ILoopOperation)op, rest, s),
        [OperationKind.Empty] = (op, rest, s) => Run(rest, s),
    };

    /// <summary>A throw ends the path; anything else is run for its effect.</summary>
    static Node Expression(IOperation op, ImmutableList<IOperation> rest, Scope s) =>
        op is IThrowOperation ? s.Result(Lower.Expr(op, s)) : Run(rest, Effect(op, s));

    static Node Return(IReturnOperation op, Scope s)
    {
        var value = Lower.Expr(op.ReturnedValue ?? throw new UnsupportedException("a return without a value"), s);
        return s.InLoop ? new Node("ret", value) : s.Result(value);
    }

    /// <summary>In a loop body, <c>continue</c> and <c>break</c> are ends.</summary>
    static Node Branch(IBranchOperation op, Scope s) => (op.BranchKind, s.InLoop) switch
    {
        (BranchKind.Continue, true) => Leaf("next", s),
        (BranchKind.Break, true) => Leaf("break", s),
        _ => throw new UnsupportedException($"a {op.BranchKind} outside a loop body"),
    };

    /// <summary>Where a loop body ends, and what the locals it changes hold there.</summary>
    static Node Leaf(string kind, Scope s) => new(kind, s.Accumulators.Select(a => s.Get(a)).ToImmutableArray());

    static Scope Declare(IVariableDeclarationGroupOperation group, Scope s)
    {
        foreach (var declarator in group.Declarations.SelectMany(d => d.Declarators))
        {
            var init = declarator.Initializer?.Value ?? declarator.GetVariableInitializer()?.Value;
            if (init is not null)
            {
                s = s.Assign(declarator.Symbol, Value(init, s));
            }
        }
        return s;
    }

    /// <summary>A value assigned: an empty builder, list or dictionary starts a value being built.</summary>
    public static Node Value(IOperation op, Scope s)
    {
        if (op is IObjectCreationOperation { Arguments.Length: 0, Initializer: null } creation)
        {
            var type = creation.Type?.OriginalDefinition.ToDisplayString();
            switch (type)
            {
                case "System.Text.StringBuilder":
                    return new Node("builder", Canon.Concat([]));
                case "System.Collections.Generic.List<T>":
                    return new Node("list");
                case "System.Collections.Generic.Dictionary<TKey, TValue>":
                    return new Node("dict");
            }
        }
        return Lower.Expr(op, s);
    }

    /// <summary>A statement for its effect: an assignment, or a value being built changed.</summary>
    static Scope Effect(IOperation op, Scope s) => op switch
    {
        ISimpleAssignmentOperation { Target: ILocalReferenceOperation local } set => s.Assign(local.Local, Value(set.Value, s)),
        ISimpleAssignmentOperation { Target: IPropertyReferenceOperation { Arguments.Length: 1 } indexer } set => Store(indexer, set.Value, s),
        ICompoundAssignmentOperation { Target: ILocalReferenceOperation local } compound => s.Set(local.Local, Compound(compound, s)),
        IIncrementOrDecrementOperation { Target: ILocalReferenceOperation local } step => s.Set(local.Local, Step(step, s)),
        IInvocationOperation call when Root(call) is { } root => s.Set(root, Lower.Expr(call, s)),
        _ => throw new UnsupportedException($"{op.Kind} as a statement"),
    };

    /// <summary><c>d[k] = v</c> on a dictionary being built.</summary>
    static Scope Store(IPropertyReferenceOperation indexer, IOperation value, Scope s)
    {
        if (indexer.Instance is not ILocalReferenceOperation local || Calls.Kind(s.Get(local.Local)) != "dict")
        {
            throw new UnsupportedException("an indexer assignment");
        }
        var dict = s.Get(local.Local);
        var key = Lower.Expr(indexer.Arguments[0].Value, s);
        return s.Set(local.Local, Canon.Make("set", dict, key, Lower.Expr(value, s)));
    }

    static Node Compound(ICompoundAssignmentOperation op, Scope s)
    {
        var old = Lower.Expr(op.Target, s);
        var value = Lower.Expr(op.Value, s);
        Canon.Rule("x op= y → x = x op y");
        if (op.Type?.SpecialType == SpecialType.System_String && op.OperatorKind == BinaryOperatorKind.Add)
        {
            return Canon.Concat([old, Canon.Part(value)]);
        }
        var symbol = op.OperatorKind switch
        {
            BinaryOperatorKind.Add => "+",
            BinaryOperatorKind.Subtract => "-",
            BinaryOperatorKind.Multiply => "*",
            _ => throw new UnsupportedException($"{op.OperatorKind}="),
        };
        return Canon.Make($"op:{symbol}:{Types.Name(op.Type)}", old, value);
    }

    static Node Step(IIncrementOrDecrementOperation op, Scope s)
    {
        Canon.Rule("x++ → x = x + 1");
        var type = Types.Name(op.Type);
        var symbol = op.Kind == OperationKind.Increment ? "+" : "-";
        return Canon.Make($"op:{symbol}:{type}", Lower.Expr(op.Target, s), Node.Const(type, "1"));
    }

    /// <summary>The local a call chain such as <c>sb.Append(a).Append(b)</c> changes.</summary>
    static ILocalSymbol? Root(IInvocationOperation call)
    {
        IOperation? at = call;
        while (at is IInvocationOperation invocation)
        {
            at = invocation.Instance;
        }
        return (at as ILocalReferenceOperation)?.Local;
    }

    /// <summary>An if: both branches go on with the rest of the body, each knowing the test's outcome.</summary>
    static Node If(IConditionalOperation op, ImmutableList<IOperation> rest, Scope s)
    {
        var then = rest.Insert(0, op.WhenTrue);
        var otherwise = op.WhenFalse is null ? rest : rest.Insert(0, op.WhenFalse);
        if (TryGet(op.Condition, s) is var (test, found))
        {
            return Fork(test, s, inner => Run(then, found(inner)), inner => Run(otherwise, inner));
        }
        var condition = Canon.Atom(Lower.Expr(op.Condition, s));
        return Fork(condition, s, inner => Run(then, inner), inner => Run(otherwise, inner));
    }

    /// <summary><c>d.TryGetValue(k, out var v)</c>: the test "d has k", and <c>v</c> set where it holds.</summary>
    static (Node Test, Func<Scope, Scope> Found)? TryGet(IOperation condition, Scope s)
    {
        if (TryGetCall(condition, s) is not { } call || OutLocal(call.Arguments[1].Value) is not { } target)
        {
            return null;
        }
        var dict = Lower.Expr(call.Instance!, s);
        var key = Lower.Expr(call.Arguments[0].Value, s);
        return (Calls.Has(dict, key), inner => inner.Set(target, Calls.Get(dict, key)));
    }

    /// <summary>A <c>TryGetValue(k, out v)</c> call on a dictionary being built.</summary>
    static IInvocationOperation? TryGetCall(IOperation condition, Scope s) =>
        condition is IInvocationOperation { TargetMethod.Name: "TryGetValue", Instance: { } instance, Arguments.Length: 2 } call
        && Calls.Kind(Lower.Expr(instance, s)) == "dict"
            ? call
            : null;

    /// <summary>The local an <c>out</c> argument writes: <c>out var v</c> or <c>out v</c>.</summary>
    static ILocalSymbol? OutLocal(IOperation argument) => argument switch
    {
        IDeclarationExpressionOperation { Expression: ILocalReferenceOperation declared } => declared.Local,
        ILocalReferenceOperation local => local.Local,
        _ => null,
    };

    /// <summary>Runs on down a decision tree: at each test, both ways, each knowing the outcome.</summary>
    public static Node Fork(Node tree, Scope s, Func<Scope, Node> whenTrue, Func<Scope, Node> whenFalse)
    {
        if (tree.Equals(Node.True))
        {
            return whenTrue(s);
        }
        if (tree.Equals(Node.False))
        {
            return whenFalse(s);
        }
        var test = tree.Kids[0];
        if (s.Facts.TryGetValue(test.Key, out var known))
        {
            return Fork(tree.Kids[known ? 1 : 2], s, whenTrue, whenFalse);
        }
        var then = Fork(tree.Kids[1], s.Know(test, true), whenTrue, whenFalse);
        var otherwise = Fork(tree.Kids[2], s.Know(test, false), whenTrue, whenFalse);
        return Canon.Choose(Canon.Atom(test), then, otherwise);
    }
}
