using Microsoft.CodeAnalysis.Operations;

namespace DgfDup;

/// <summary>Patterns (<c>is null</c>, <c>is &gt; 0</c>, <c>switch</c> arms) as the tests they make.</summary>
public static class Patterns
{
    public static Node Test(Node value, IPatternOperation pattern, Scope s) =>
        Kinds.TryGetValue(pattern.Kind, out var test)
            ? test(value, pattern, s)
            : throw new UnsupportedException($"a {pattern.Kind} pattern");

    static readonly Dictionary<Microsoft.CodeAnalysis.OperationKind, Func<Node, IPatternOperation, Scope, Node>> Kinds = new()
    {
        [Microsoft.CodeAnalysis.OperationKind.ConstantPattern] = (value, p, s) =>
            Canon.Compare("==", Types.Name(p.InputType), value, Lower.Expr(((IConstantPatternOperation)p).Value, s)),
        [Microsoft.CodeAnalysis.OperationKind.RelationalPattern] = (value, p, s) =>
            Canon.Compare(Relation(((IRelationalPatternOperation)p).OperatorKind), Types.Name(p.InputType), value, Lower.Expr(((IRelationalPatternOperation)p).Value, s)),
        [Microsoft.CodeAnalysis.OperationKind.NegatedPattern] = (value, p, s) => Canon.Not(Test(value, ((INegatedPatternOperation)p).Pattern, s)),
        [Microsoft.CodeAnalysis.OperationKind.BinaryPattern] = (value, p, s) => Binary(value, (IBinaryPatternOperation)p, s),
        [Microsoft.CodeAnalysis.OperationKind.DiscardPattern] = (value, p, s) => Node.True,
        [Microsoft.CodeAnalysis.OperationKind.RecursivePattern] = (value, p, s) => Recursive(value, (IRecursivePatternOperation)p, s),
    };

    static Node Binary(Node value, IBinaryPatternOperation pattern, Scope s)
    {
        var left = Test(value, pattern.LeftPattern, s);
        var right = Test(value, pattern.RightPattern, s);
        return pattern.OperatorKind == BinaryOperatorKind.And ? Canon.And(left, right) : Canon.Or(left, right);
    }

    /// <summary><c>x is { A: 1, B.C: &gt; 2 }</c>: not null (or of the type), and each member's pattern.</summary>
    static Node Recursive(Node value, IRecursivePatternOperation pattern, Scope s)
    {
        if (pattern.DeclaredSymbol is not null || !pattern.DeconstructionSubpatterns.IsEmpty)
        {
            throw new UnsupportedException("a pattern that declares or deconstructs");
        }
        var sameType = Microsoft.CodeAnalysis.SymbolEqualityComparer.Default.Equals(pattern.MatchedType.WithNullableAnnotation(Microsoft.CodeAnalysis.NullableAnnotation.None), pattern.InputType.WithNullableAnnotation(Microsoft.CodeAnalysis.NullableAnnotation.None));
        var test = sameType
            ? Canon.Not(Canon.Compare("==", Types.Name(pattern.InputType), value, Node.Null))
            : Canon.Atom(new Node("is:" + Types.Name(pattern.MatchedType), value));
        foreach (var sub in pattern.PropertySubpatterns)
        {
            test = Canon.And(test, Test(Read(sub.Member, value, s), sub.Pattern, s));
        }
        return test;
    }

    /// <summary>The member a property pattern reads, off the value matched.</summary>
    static Node Read(Microsoft.CodeAnalysis.IOperation member, Node value, Scope s) => member switch
    {
        IInstanceReferenceOperation => value,
        IPropertyReferenceOperation { Arguments.Length: 0, Instance: { } instance } p when Types.IsCount(p.Property) => Canon.Count(Read(instance, value, s), null),
        IPropertyReferenceOperation { Arguments.Length: 0, Instance: { } instance } p => Canon.Make("member:" + Types.Symbol(p.Property), Read(instance, value, s)),
        IFieldReferenceOperation { Instance: { } instance } f => Canon.Make("member:" + Types.Symbol(f.Field), Read(instance, value, s)),
        _ => throw new UnsupportedException("a property pattern on something other than a member"),
    };

    static string Relation(BinaryOperatorKind kind) => kind switch
    {
        BinaryOperatorKind.LessThan => "<",
        BinaryOperatorKind.LessThanOrEqual => "<=",
        BinaryOperatorKind.GreaterThan => ">",
        _ => ">=",
    };

    /// <summary>A switch expression as a chain of conditionals, arm by arm.</summary>
    public static Node Switch(ISwitchExpressionOperation op, Scope s)
    {
        var value = Lower.Expr(op.Value, s);
        Canon.Rule("switch → conditionals");
        Node result = new("throw:System.Runtime.CompilerServices.SwitchExpressionException");
        foreach (var arm in op.Arms.Reverse())
        {
            var test = Test(value, arm.Pattern, s);
            if (arm.Guard is not null)
            {
                test = Canon.And(test, Lower.Expr(arm.Guard, s));
            }
            result = Canon.Choose(test, Lower.Expr(arm.Value, s), result);
        }
        return result;
    }
}
