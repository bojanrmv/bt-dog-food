using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace DgfDup;

/// <summary>A run of lines in a file, as dgf-dup names a copy.</summary>
public sealed record Region(string File, int StartLine, int EndLine);

/// <summary>
/// What a copy needs and gives if it is extracted into a method, from the
/// compiler's data-flow analysis: the locals and parameters it reads that
/// were set before it (the new method's parameters), those it sets that are
/// read after it (what it returns), and the jumps out of it.
/// </summary>
public sealed record Signature(List<string> Parameters, List<string> Returns, List<string> Exits, bool UsesThis);

/// <summary>
/// With <c>--regions</c>: after the report, reads the regions dgf-dup found
/// as JSON on stdin, and writes each one's <see cref="Signature"/> (or null
/// where the lines are not whole statements of one block).
/// </summary>
public static class Regions
{
    public static void Answer(Workspace workspace, string root)
    {
        var regions = JsonSerializer.Deserialize<List<Region>>(Console.In.ReadToEnd(), Options) ?? [];
        var trees = workspace.Scanned.ToDictionary(
            s => Path.GetRelativePath(root, s.Tree.FilePath).Replace('\\', '/'),
            s => s,
            StringComparer.OrdinalIgnoreCase);
        var answers = regions.AsParallel().AsOrdered()
            .Select(r => trees.TryGetValue(r.File, out var found) ? Of(found.Tree, found.Compilation, r) : null)
            .ToList();
        Console.WriteLine(JsonSerializer.Serialize(answers, Options));
    }

    static readonly JsonSerializerOptions Options = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
    };

    static Signature? Of(SyntaxTree tree, Compilation compilation, Region region)
    {
        var statements = Statements(tree, region);
        if (statements is not [var first, .., var last])
        {
            return null;
        }
        var model = compilation.GetSemanticModel(tree);
        var flow = model.AnalyzeDataFlow(first, last);
        var control = model.AnalyzeControlFlow(first, last);
        if (flow is not { Succeeded: true } || control is not { Succeeded: true })
        {
            return null;
        }
        return new Signature(
            [.. flow.DataFlowsIn.Where(s => s is not IParameterSymbol { IsThis: true }).Select(Declared)],
            [.. flow.DataFlowsOut.Select(Declared)],
            [.. control.ExitPoints.Select(e => Microsoft.CodeAnalysis.CSharp.CSharpExtensions.Kind(e).ToString().Replace("Statement", "").ToLowerInvariant()).Distinct()],
            UsesMembers(statements, model));
    }

    /// <summary>The statements of one block that lie within the region's lines, first to last.</summary>
    static List<StatementSyntax> Statements(SyntaxTree tree, Region region)
    {
        var text = tree.GetText();
        if (region.StartLine < 1 || region.EndLine > text.Lines.Count)
        {
            return [];
        }
        var span = Microsoft.CodeAnalysis.Text.TextSpan.FromBounds(text.Lines[region.StartLine - 1].Start, text.Lines[region.EndLine - 1].End);
        // The block most of whose statements the region holds, whole: not a block nested in them.
        var block = tree.GetRoot().DescendantNodes(span).OfType<BlockSyntax>()
            .Where(b => b.Statements.Any(s => span.Contains(s.Span))
                && b.Statements.Count(s => span.Contains(s.Span)) == b.Statements.Count(s => s.Span.IntersectsWith(span)))
            .OrderByDescending(b => b.Statements.Count(s => span.Contains(s.Span)))
            .ThenBy(b => b.Span.Length)
            .FirstOrDefault();
        return block?.Statements.Where(s => span.Contains(s.Span)).ToList() ?? [];
    }

    /// <summary>Whether the statements use <c>this</c>, or a field, property or method of it: then the method can not be static.</summary>
    static bool UsesMembers(List<StatementSyntax> statements, SemanticModel model) =>
        statements.SelectMany(s => s.DescendantNodesAndSelf()).Any(n => n is ThisExpressionSyntax or BaseExpressionSyntax || Implicit(n, model));

    /// <summary>A name that is a member of <c>this</c> without <c>this.</c>: not after a dot, and an instance member.</summary>
    static bool Implicit(SyntaxNode node, SemanticModel model) =>
        node is IdentifierNameSyntax name
        && !(node.Parent is MemberAccessExpressionSyntax access && access.Name == name)
        && Instance(model.GetSymbolInfo(name).Symbol);

    static bool Instance(ISymbol? symbol) =>
        symbol is { IsStatic: false, Kind: SymbolKind.Field or SymbolKind.Property or SymbolKind.Method or SymbolKind.Event }
        && symbol is not IMethodSymbol { MethodKind: MethodKind.LocalFunction };

    static string Declared(ISymbol symbol) => symbol switch
    {
        ILocalSymbol local => $"{local.Type.ToDisplayString(SymbolDisplayFormat.MinimallyQualifiedFormat)} {local.Name}",
        IParameterSymbol parameter => $"{parameter.Type.ToDisplayString(SymbolDisplayFormat.MinimallyQualifiedFormat)} {parameter.Name}",
        _ => symbol.Name,
    };
}
