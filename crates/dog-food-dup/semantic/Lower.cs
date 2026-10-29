using System.Collections.Immutable;
using System.Globalization;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.Operations;

namespace DgfDup;

/// <summary>
/// Expressions (Roslyn operations) as canonical nodes. Symbols decide
/// meaning: a member is its declaring type and name, a call its method with
/// parameter types, so overloads and same-named members of other types stay
/// apart. One handler per kind of operation.
/// </summary>
public static class Lower
{
    public static Node Expr(IOperation op, Scope s)
    {
        if (s.Hole?.Invoke(op) is { } hole)
        {
            return hole;
        }
        if (op.ConstantValue.HasValue && op.Type is not null)
        {
            return Constant(op.Type, op.ConstantValue.Value);
        }
        return Handlers.TryGetValue(op.Kind, out var handle)
            ? handle(op, s)
            : throw new UnsupportedException($"{op.Kind} in an expression");
    }

    static readonly Dictionary<OperationKind, Func<IOperation, Scope, Node>> Handlers = new()
    {
        [OperationKind.ParameterReference] = (op, s) => Parameter(((IParameterReferenceOperation)op).Parameter, s),
        [OperationKind.LocalReference] = (op, s) => s.Get(((ILocalReferenceOperation)op).Local),
        [OperationKind.InstanceReference] = (op, s) => new Node("this:" + Types.Name(op.Type)),
        [OperationKind.PropertyReference] = (op, s) => Property((IPropertyReferenceOperation)op, s),
        [OperationKind.FieldReference] = (op, s) => Field((IFieldReferenceOperation)op, s),
        [OperationKind.ArrayElementReference] = (op, s) => ArrayElement((IArrayElementReferenceOperation)op, s),
        [OperationKind.Conversion] = (op, s) => Conversion((IConversionOperation)op, s),
        [OperationKind.Binary] = (op, s) => Binary((IBinaryOperation)op, s),
        [OperationKind.Unary] = (op, s) => Unary((IUnaryOperation)op, s),
        [OperationKind.Conditional] = (op, s) => Conditional((IConditionalOperation)op, s),
        [OperationKind.Coalesce] = (op, s) => Coalesce((ICoalesceOperation)op, s),
        [OperationKind.ConditionalAccess] = (op, s) => ConditionalAccess((IConditionalAccessOperation)op, s),
        [OperationKind.ConditionalAccessInstance] = (op, s) => s.Accessed ?? throw new UnsupportedException("?. outside an access"),
        [OperationKind.IsPattern] = (op, s) => Patterns.Test(Expr(((IIsPatternOperation)op).Value, s), ((IIsPatternOperation)op).Pattern, s),
        [OperationKind.SwitchExpression] = (op, s) => Patterns.Switch((ISwitchExpressionOperation)op, s),
        [OperationKind.Invocation] = (op, s) => Calls.Invocation((IInvocationOperation)op, s),
        [OperationKind.InterpolatedString] = (op, s) => Interpolated((IInterpolatedStringOperation)op, s),
        [OperationKind.AnonymousFunction] = (op, s) => Lambda((IAnonymousFunctionOperation)op, s),
        [OperationKind.DelegateCreation] = (op, s) => Expr(((IDelegateCreationOperation)op).Target, s),
        [OperationKind.Argument] = (op, s) => Expr(((IArgumentOperation)op).Value, s),
        [OperationKind.ObjectCreation] = (op, s) => Creation((IObjectCreationOperation)op, s),
        [OperationKind.DefaultValue] = (op, s) => new Node("default:" + Types.Name(op.Type)),
        [OperationKind.Throw] = (op, s) => new Node("throw:" + Types.Name(((IThrowOperation)op).Exception?.Type)),
        [OperationKind.Parenthesized] = (op, s) => Expr(((IParenthesizedOperation)op).Operand, s),
        // An await is a step in order: the awaited value, kept as it is.
        [OperationKind.Await] = (op, s) => Canon.Make("await", Expr(((IAwaitOperation)op).Operation, s)),
        [OperationKind.AnonymousObjectCreation] = (op, s) => new Node("anonymous", Initialized(((IAnonymousObjectCreationOperation)op).Initializers, s)),
        [OperationKind.CollectionExpression] = (op, s) => Collection((ICollectionExpressionOperation)op, s),
    };

    public static Node Constant(ITypeSymbol type, object? value) =>
        value is null ? Node.Null : Node.Const(Types.Name(type), Text(value));

    /// <summary>A constant as text that is equal when the values are.</summary>
    static string Text(object value) => value switch
    {
        string str => Canon.Quote(str),
        bool b => b.ToString().ToLowerInvariant(),
        decimal d => d.ToString("G29", CultureInfo.InvariantCulture),
        IFormattable f => f.ToString(null, CultureInfo.InvariantCulture),
        _ => value.ToString()!,
    };

    static Node Parameter(IParameterSymbol parameter, Scope s)
    {
        if (s.Values.TryGetValue(parameter, out var bound))
        {
            return bound;
        }
        var index = s.Parameters.IndexOf(parameter, SymbolEqualityComparer.Default);
        // A primary constructor's parameter, captured like a field: by name and type.
        return index < 0
            ? new Node($"self:{parameter.Name}:{Types.Name(parameter.Type)}")
            : new Node($"param:{index}:{Types.Name(parameter.Type)}");
    }

    static Node Property(IPropertyReferenceOperation op, Scope s)
    {
        var instance = op.Instance is null ? null : Expr(op.Instance, s);
        if (op.Arguments.Length == 1 && instance is not null)
        {
            return Indexed(instance, op.Arguments[0].Value, s, op.Property);
        }
        if (instance is not null && Types.IsCount(op.Property))
        {
            return Canon.Count(instance, null);
        }
        return Member(op.Property, instance);
    }

    /// <summary>
    /// A field of this object is read by name and type, so the same code over
    /// an injected <c>_db</c> in two classes matches; other fields by symbol.
    /// </summary>
    static Node Field(IFieldReferenceOperation op, Scope s)
    {
        if (op.Instance is IInstanceReferenceOperation)
        {
            return new Node($"self:{op.Field.Name.TrimStart('_')}:{Types.Name(op.Field.Type)}");
        }
        return Member(op.Field, op.Instance is null ? null : Expr(op.Instance, s));
    }

    static Node Member(ISymbol member, Node? instance) =>
        instance is null
            ? new Node("member:" + Types.Symbol(member))
            : Canon.Make("member:" + Types.Symbol(member), instance);

    static Node ArrayElement(IArrayElementReferenceOperation op, Scope s)
    {
        if (op.Indices.Length != 1)
        {
            throw new UnsupportedException("arrays of more than one dimension");
        }
        return Indexed(Expr(op.ArrayReference, s), op.Indices[0], s, null);
    }

    /// <summary><c>xs[i]</c>: in an index loop over <c>xs</c>, the element.</summary>
    static Node Indexed(Node instance, IOperation index, Scope s, IPropertySymbol? indexer)
    {
        if (LoopElement(instance, index, s) is { } element)
        {
            return element;
        }
        if (Calls.Kind(instance) == "dict")
        {
            return Calls.Get(instance, Expr(index, s));
        }
        return Canon.Make("index:" + (indexer is null ? "array" : Types.Symbol(indexer)), instance, Expr(index, s));
    }

    /// <summary>The element when <c>xs[i]</c> reads the collection an index loop walks, at its index.</summary>
    static Node? LoopElement(Node instance, IOperation index, Scope s) =>
        s.Index is { } loop && index is ILocalReferenceOperation local
        && SymbolEqualityComparer.Default.Equals(local.Local, loop.Index) && instance.Equals(loop.Source)
            ? loop.Element
            : null;

    static Node Conversion(IConversionOperation op, Scope s)
    {
        var operand = Expr(op.Operand, s);
        if (!op.Conversion.IsNumeric || op.Operand.Type is null || SymbolEqualityComparer.Default.Equals(op.Operand.Type, op.Type))
        {
            return operand;
        }
        // A constant converted is the constant of the new type: 0 as a decimal is 0m.
        return Canon.Make("conv:" + Types.Name(op.Type), operand);
    }

    static Node Binary(IBinaryOperation op, Scope s)
    {
        if (op.OperatorMethod is { MethodKind: MethodKind.UserDefinedOperator } method && !Ordered(op))
        {
            return Canon.Make("call:" + Types.Symbol(method), Expr(op.LeftOperand, s), Expr(op.RightOperand, s));
        }
        var left = Expr(op.LeftOperand, s);
        var right = Expr(op.RightOperand, s);
        if (Comparisons.TryGetValue(op.OperatorKind, out var comparison))
        {
            return Canon.Compare(comparison, Types.Name(op.LeftOperand.Type), left, right);
        }
        return Logical(op, left, right) ?? Arithmetic(op, left, right);
    }

    /// <summary>A comparison of dates, times or durations (ordered like numbers), or an equality of a framework type.</summary>
    static bool Ordered(IBinaryOperation op) =>
        Comparisons.ContainsKey(op.OperatorKind) && Numbers.IsTotal(Types.Name(op.LeftOperand.Type))
        || op.OperatorKind is BinaryOperatorKind.Equals or BinaryOperatorKind.NotEquals && IsFramework(op.OperatorMethod);

    /// <summary>An operator of a framework type (<c>Guid ==</c>): it means what it says.</summary>
    static bool IsFramework(IMethodSymbol? method) =>
        method?.ContainingType.ContainingNamespace?.ToDisplayString() == "System";

    static Node? Logical(IBinaryOperation op, Node left, Node right) => op.OperatorKind switch
    {
        BinaryOperatorKind.ConditionalAnd => Canon.And(left, right),
        BinaryOperatorKind.ConditionalOr => Canon.Or(left, right),
        _ => null,
    };

    /// <summary><c>+</c> on strings concatenates; the other operators keep their symbol and type.</summary>
    static Node Arithmetic(IBinaryOperation op, Node left, Node right)
    {
        if (op.OperatorKind == BinaryOperatorKind.Add && op.Type?.SpecialType == SpecialType.System_String)
        {
            return Canon.Concat([Canon.Part(left), Canon.Part(right)]);
        }
        var symbol = Symbols.GetValueOrDefault(op.OperatorKind, op.OperatorKind.ToString());
        return Canon.Make($"op:{symbol}:{Types.Name(op.Type)}", left, right);
    }

    static readonly Dictionary<BinaryOperatorKind, string> Comparisons = new()
    {
        [BinaryOperatorKind.Equals] = "==",
        [BinaryOperatorKind.NotEquals] = "!=",
        [BinaryOperatorKind.LessThan] = "<",
        [BinaryOperatorKind.LessThanOrEqual] = "<=",
        [BinaryOperatorKind.GreaterThan] = ">",
        [BinaryOperatorKind.GreaterThanOrEqual] = ">=",
    };

    static readonly Dictionary<BinaryOperatorKind, string> Symbols = new()
    {
        [BinaryOperatorKind.Add] = "+",
        [BinaryOperatorKind.Subtract] = "-",
        [BinaryOperatorKind.Multiply] = "*",
        [BinaryOperatorKind.Divide] = "/",
        [BinaryOperatorKind.Remainder] = "%",
        [BinaryOperatorKind.And] = "&",
        [BinaryOperatorKind.Or] = "|",
        [BinaryOperatorKind.ExclusiveOr] = "^",
    };

    static Node Unary(IUnaryOperation op, Scope s)
    {
        var operand = Expr(op.Operand, s);
        return op.OperatorKind switch
        {
            UnaryOperatorKind.Not => Canon.Not(operand),
            UnaryOperatorKind.Plus => operand,
            _ => Canon.Make($"op:{op.OperatorKind}:{Types.Name(op.Type)}", operand),
        };
    }

    static Node Conditional(IConditionalOperation op, Scope s)
    {
        if (op.WhenFalse is null)
        {
            throw new UnsupportedException("if without else as an expression");
        }
        return Canon.Choose(Canon.Atom(Expr(op.Condition, s)), Expr(op.WhenTrue, s), Expr(op.WhenFalse, s));
    }

    /// <summary><c>a ?? b</c> is <c>a == null ? b : a</c>.</summary>
    static Node Coalesce(ICoalesceOperation op, Scope s)
    {
        var value = Expr(op.Value, s);
        Canon.Rule("?? → conditional");
        return Canon.Choose(Canon.Compare("==", Types.Name(op.Value.Type), value, Node.Null), Expr(op.WhenNull, s), value);
    }

    /// <summary><c>a?.B</c> is <c>a == null ? null : a.B</c>.</summary>
    static Node ConditionalAccess(IConditionalAccessOperation op, Scope s)
    {
        var target = Expr(op.Operation, s);
        Canon.Rule("?. → conditional");
        var read = Expr(op.WhenNotNull, s with { Accessed = target });
        return Canon.Choose(Canon.Compare("==", Types.Name(op.Operation.Type), target, Node.Null), Node.Null, read);
    }

    static Node Interpolated(IInterpolatedStringOperation op, Scope s)
    {
        var parts = op.Parts.Select(part => part switch
        {
            IInterpolatedStringTextOperation text => Expr(text.Text, s),
            IInterpolationOperation { Alignment: null, FormatString: null } hole => Canon.Part(Expr(hole.Expression, s)),
            _ => throw new UnsupportedException("interpolation with a format"),
        });
        Canon.Rule("interpolation → concatenation");
        return Canon.Concat(parts.ToList());
    }

    /// <summary>A lambda binds its parameter as the next element: <c>lambda:d(body)</c>.</summary>
    public static Node Lambda(IAnonymousFunctionOperation op, Scope s)
    {
        if (op.Symbol.Parameters.Length != 1)
        {
            throw new UnsupportedException("a lambda of other than one parameter");
        }
        var element = new Node($"elem:{s.Depth}");
        var inner = s.Set(op.Symbol.Parameters[0], element) with { Depth = s.Depth + 1 };
        var body = op.Body.Operations is [IReturnOperation { ReturnedValue: { } value }]
            ? Expr(value, inner)
            : Exec.Body(op.Body, inner);
        return Canon.Lambda($"lambda:{s.Depth}", body);
    }

    /// <summary><c>new T(a) { X = x, Y = y }</c>: the constructor's arguments, then each member set, in order.</summary>
    static Node Creation(IObjectCreationOperation op, Scope s)
    {
        var args = op.Arguments.Select(a => Expr(a.Value, s)).ToImmutableArray();
        if (op.Initializer is { } initializer)
        {
            args = args.AddRange(Initialized(initializer.Initializers, s));
        }
        return new Node("new:" + Types.Symbol(op.Constructor ?? (ISymbol?)op.Type), args);
    }

    /// <summary>Member sets of an initializer as <c>init:Member(value)</c>; <c>Add</c> calls as <c>add(value)</c>.</summary>
    static ImmutableArray<Node> Initialized(ImmutableArray<IOperation> initializers, Scope s) =>
        initializers.Select(i => i switch
        {
            ISimpleAssignmentOperation { Target: IPropertyReferenceOperation { Arguments.Length: 0 } p } set => new Node("init:" + Types.Symbol(p.Property), Expr(set.Value, s)),
            ISimpleAssignmentOperation { Target: IFieldReferenceOperation f } set => new Node("init:" + Types.Symbol(f.Field), Expr(set.Value, s)),
            IInvocationOperation { TargetMethod.Name: "Add" } add => new Node("add", add.Arguments.Select(a => Expr(a.Value, s)).ToImmutableArray()),
            _ => throw new UnsupportedException("a nested initializer"),
        }).ToImmutableArray();

    static Node Collection(ICollectionExpressionOperation op, Scope s) =>
        new("collection:" + Types.Name(op.Type), op.Elements.Select(e => e is ISpreadOperation
            ? throw new UnsupportedException("a spread in a collection expression")
            : Expr(e, s)).ToImmutableArray());
}

/// <summary>Names of types and symbols as they go into keys.</summary>
public static class Types
{
    /// <summary>C#'s own spelling (<c>int</c>, <c>decimal</c>, <c>Corpus.Order</c>); enums are marked, as they compare like integers.</summary>
    public static string Name(ITypeSymbol? type) => type switch
    {
        null => "?",
        { TypeKind: TypeKind.Enum } => "enum:" + type.ToDisplayString(),
        _ => type.ToDisplayString(),
    };

    public static string Symbol(ISymbol? symbol) => symbol?.ToDisplayString() ?? "?";

    /// <summary><c>Count</c> of a collection, or <c>Length</c> of an array.</summary>
    public static bool IsCount(IPropertySymbol property) =>
        property.Name == "Count" && property.Type.SpecialType == SpecialType.System_Int32 && IsCollection(property.ContainingType)
        || property.Name == "Length" && property.ContainingType.SpecialType == SpecialType.System_Array;

    static bool IsCollection(ITypeSymbol type) =>
        type.AllInterfaces.Append(type as INamedTypeSymbol).Any(i => i?.OriginalDefinition.ToDisplayString() is
            "System.Collections.Generic.IReadOnlyCollection<T>" or "System.Collections.Generic.ICollection<T>");
}
