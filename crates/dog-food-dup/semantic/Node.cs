using System.Collections.Immutable;
using System.Text;

namespace DgfDup;

/// <summary>
/// One node of a canonical expression: a head and its children. Two methods
/// do the same thing when their expressions have the same <see cref="Key"/>.
/// Heads name what a node is, with everything that decides its meaning:
/// <c>const:decimal:5</c>, <c>param:0:Corpus.Order</c>, <c>member:Corpus.Line.Price</c>,
/// <c>op:+:decimal</c>, <c>cond</c>, <c>lambda:0</c>, <c>elem:0</c>, <c>sum</c>, <c>any</c>.
/// </summary>
public sealed class Node : IEquatable<Node>
{
    public string Head { get; }
    public ImmutableArray<Node> Kids { get; }
    string? key;

    public Node(string head, params Node[] kids)
        : this(head, ImmutableArray.Create(kids)) { }

    /// <summary>Most nodes in one expression: past it a method is left out, as
    /// lifting conditions out of every expression can grow a tree exponentially.</summary>
    public const int Largest = 3000;

    public Node(string head, ImmutableArray<Node> kids)
    {
        Head = head;
        Kids = kids;
        var size = 1;
        foreach (var kid in kids)
        {
            size += kid.Size;
        }
        Size = size > Largest ? throw new UnsupportedException("an expression too big to compare") : size;
    }

    /// <summary>The canonical text: equal keys, equal meaning.</summary>
    public string Key => key ??= Kids.IsEmpty
        ? Head
        : Head + "(" + string.Join(",", Kids.Select(k => k.Key)) + ")";

    /// <summary>Nodes in the tree, for size limits.</summary>
    public int Size { get; }

    public Node With(ImmutableArray<Node> kids) =>
        kids == Kids || kids.SequenceEqual(Kids) ? this : new Node(Head, kids);

    public bool Is(string head) => Head == head;

    public bool StartsWith(string prefix) => Head.StartsWith(prefix, StringComparison.Ordinal);

    public bool Equals(Node? other) => other is not null && Key == other.Key;

    public override bool Equals(object? obj) => obj is Node other && Equals(other);

    public override int GetHashCode() => Key.GetHashCode(StringComparison.Ordinal);

    public override string ToString() => Key;

    public static readonly Node True = new("const:bool:true");
    public static readonly Node False = new("const:bool:false");
    public static readonly Node Null = new("const:null");

    public static Node Cond(Node test, Node then, Node otherwise) => new("cond", test, then, otherwise);

    public static Node Const(string type, string value) => new($"const:{type}:{value}");

    /// <summary>A readable form for reports: calls and operators as code.</summary>
    public string Pretty()
    {
        var text = new StringBuilder();
        Print(text);
        return text.ToString();
    }

    void Print(StringBuilder text)
    {
        var (kind, rest) = Split(Head);
        if (Printers.TryGetValue(kind, out var print))
        {
            print(this, rest, text);
            return;
        }
        text.Append(Short(Head));
        if (!Kids.IsEmpty)
        {
            text.Append('(');
            PrintList(text, Kids);
            text.Append(')');
        }
    }

    static (string Kind, string Tail) Split(string head)
    {
        var colon = head.IndexOf(':');
        return colon < 0 ? (head, "") : (head[..colon], head[(colon + 1)..]);
    }

    static void PrintList(StringBuilder text, IEnumerable<Node> kids)
    {
        var first = true;
        foreach (var kid in kids)
        {
            if (!first)
            {
                text.Append(", ");
            }
            kid.Print(text);
            first = false;
        }
    }

    /// <summary>The last part of a dotted name, without its parameter list.</summary>
    static string Short(string name)
    {
        var paren = name.IndexOf('(');
        var bare = paren < 0 ? name : name[..paren];
        var dot = bare.LastIndexOf('.');
        return dot < 0 ? bare : bare[(dot + 1)..];
    }

    static readonly Dictionary<string, Action<Node, string, StringBuilder>> Printers = new()
    {
        ["const"] = (n, rest, t) => t.Append(rest[(rest.IndexOf(':') + 1)..]),
        ["param"] = (n, rest, t) => t.Append("p").Append(rest[..rest.IndexOf(':')]),
        ["elem"] = (n, rest, t) => t.Append("x").Append(rest),
        ["this"] = (n, rest, t) => t.Append("this"),
        ["member"] = (n, rest, t) =>
        {
            if (n.Kids.IsEmpty)
            {
                t.Append(rest);
                return;
            }
            n.Kids[0].Print(t);
            t.Append('.').Append(Short(rest));
        },
        ["op"] = PrintOperator,
        ["lambda"] = (n, rest, t) =>
        {
            t.Append('x').Append(rest).Append(" => ");
            n.Kids[0].Print(t);
        },
        ["cond"] = (n, rest, t) =>
        {
            t.Append('(');
            n.Kids[0].Print(t);
            t.Append(" ? ");
            n.Kids[1].Print(t);
            t.Append(" : ");
            n.Kids[2].Print(t);
            t.Append(')');
        },
        ["call"] = (n, rest, t) =>
        {
            t.Append(Short(rest)).Append('(');
            PrintList(t, n.Kids);
            t.Append(')');
        },
    };

    static void PrintOperator(Node n, string rest, StringBuilder t)
    {
        var symbol = rest[..rest.IndexOf(':')];
        if (n.Kids.Length == 1)
        {
            t.Append(symbol);
            n.Kids[0].Print(t);
            return;
        }
        t.Append('(');
        n.Kids[0].Print(t);
        t.Append(' ').Append(symbol).Append(' ');
        n.Kids[1].Print(t);
        t.Append(')');
    }
}

/// <summary>A method can not be reduced: it uses something the engine does not model.</summary>
public sealed class UnsupportedException(string what) : Exception(what);
