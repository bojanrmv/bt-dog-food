// dgf-dup-cs --check: compile checks for dgf mutate, in memory.
//
// stdin, first line: {"files": [{"path": "...", "original": "..."}]}: the
// mutated files in the sandbox, and a file with each one's code without
// mutants. Then one line per check: "check". The engine reads the files again
// and writes, for each error the mutants brought in, a line
// "error\t<path>(<line>,<col>): error <id>: <message>", then "done".
// An error inside a mutant's own code (the mutated arm of its switch, or its
// copy in a probe) ends with " [dgf mutant <id>]": that mutant does not
// compile, whatever else is on its line.
//
// An error counts only when the member it is in had no error with that id and
// message without the mutants: the compiler here runs no source generators and
// no MSBuild, so some code has errors of its own, and a mutant copies the code
// it changes. A miss is safe: the build that follows finds what this does not.

using System.Text.Json;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;

namespace DgfDup;

static class Check
{
    static readonly CSharpParseOptions Parse = new(LanguageVersion.Preview);
    const string Helper = "DgfMutants.g.cs";

    public static int Run()
    {
        var request = JsonDocument.Parse(Console.In.ReadLine() ?? "{}").RootElement;
        var files = request.GetProperty("files").EnumerateArray()
            .Select(f => (Path: Path.GetFullPath(f.GetProperty("path").GetString()!), Original: f.GetProperty("original").GetString()!))
            .ToList();
        var workspace = Workspace.Load(files.Select(f => f.Path).ToList());
        var current = new Dictionary<string, (SyntaxTree Tree, CSharpCompilation Compilation)>(StringComparer.OrdinalIgnoreCase);
        var helpers = new Dictionary<CSharpCompilation, CSharpCompilation>();
        foreach (var (tree, compilation) in workspace.Scanned)
        {
            if (!helpers.TryGetValue(compilation, out var with))
            {
                with = helpers[compilation] = WithHelper(compilation, tree.FilePath);
            }
            current[Path.GetFullPath(tree.FilePath)] = (tree, with);
        }
        var known = files.Where(f => current.ContainsKey(f.Path)).ToDictionary(
            f => f.Path, f => Baseline(current[f.Path], File.ReadAllText(f.Original)), StringComparer.OrdinalIgnoreCase);
        var output = Console.Out;
        output.WriteLine("ready");
        output.Flush();
        while (Console.In.ReadLine() is { } line)
        {
            if (line.Trim() != "check")
            {
                continue;
            }
            foreach (var error in Round(current, known))
            {
                output.WriteLine("error\t" + error);
            }
            output.WriteLine("done");
            output.Flush();
        }
        return 0;
    }

    /// <summary>The compilation with dgf's switch class, which the sandbox puts in each mutated project.</summary>
    static CSharpCompilation WithHelper(CSharpCompilation compilation, string file)
    {
        var helper = Project.Ancestors(Path.GetDirectoryName(file)!)
            .Select(d => Path.Combine(d, Helper)).FirstOrDefault(File.Exists);
        return helper is null || compilation.SyntaxTrees.Any(t => t.FilePath == helper)
            ? compilation
            : compilation.AddSyntaxTrees(CSharpSyntaxTree.ParseText(File.ReadAllText(helper), Parse, helper));
    }

    /// <summary>The errors of the file's code without mutants, by member.</summary>
    static HashSet<string> Baseline((SyntaxTree Tree, CSharpCompilation Compilation) at, string original)
    {
        var tree = CSharpSyntaxTree.ParseText(original, Parse, at.Tree.FilePath);
        var compilation = at.Compilation.ReplaceSyntaxTree(at.Tree, tree);
        var errors = Errors(compilation, tree).ToList();
        if (Environment.GetEnvironmentVariable("DGF_CHECK_DEBUG") is not null)
        {
            foreach (var (key, line) in errors)
            {
                Console.Error.WriteLine($"baseline	{key.Replace('\0', '|')}	{line}");
            }
        }
        return errors.Select(e => e.Key).ToHashSet();
    }

    /// <summary>Each mutated file's text as the last round read it.</summary>
    static readonly Dictionary<string, string> Texts = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>Each mutated file's errors from the mutants, as of the last round that checked its project.</summary>
    static readonly Dictionary<string, List<string>> Found = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>Each mutated file's code outside its members' bodies, as of the last round.</summary>
    static readonly Dictionary<string, string> Shapes = new(StringComparer.OrdinalIgnoreCase);

    /// <summary>Reads the mutated files again; the errors the mutants brought in. Only a file whose
    /// text changed is parsed again. A change inside members' bodies can only bring errors to its own
    /// file, so then that file is checked again, with the files that had errors (they may be gone);
    /// a change outside the bodies (a constant, a field's value) can reach every file of the project,
    /// and all of them are checked again. The others keep the errors they had.</summary>
    static List<string> Round(Dictionary<string, (SyntaxTree Tree, CSharpCompilation Compilation)> current, Dictionary<string, HashSet<string>> known)
    {
        var (replaced, reshaped, trees) = Reread(current, known);
        Retarget(current, replaced);
        var whole = reshaped.Select(compilation => replaced[compilation]).ToHashSet();
        var newer = replaced.Values.ToHashSet();
        var stale = known.Keys.Where(path => newer.Contains(current[path].Compilation)
            && (whole.Contains(current[path].Compilation) || trees.Contains(path)
                || (Found.TryGetValue(path, out var had) && had.Count > 0))).ToList();
        var results = new List<string>[stale.Count];
        Parallel.For(0, stale.Count, i =>
        {
            var path = stale[i];
            var (tree, compilation) = current[path];
            results[i] = Errors(compilation, tree).Where(e => !known[path].Contains(e.Key)).Select(e => e.Line).ToList();
        });
        for (var i = 0; i < stale.Count; i++)
        {
            Found[stale[i]] = results[i];
        }
        return known.Keys.SelectMany(path => Found.TryGetValue(path, out var errors) ? errors : []).ToList();
    }

    /// <summary>Parses the files whose text changed and puts them in their projects' compilations
    /// (files of one project share one: all are replaced before any is checked). Returns each
    /// compilation that got a newer one, those of them with a change outside members' bodies, and
    /// the files that changed.</summary>
    static (Dictionary<CSharpCompilation, CSharpCompilation> Replaced, HashSet<CSharpCompilation> Reshaped, HashSet<string> Changed) Reread(
        Dictionary<string, (SyntaxTree Tree, CSharpCompilation Compilation)> current, Dictionary<string, HashSet<string>> known)
    {
        var replaced = new Dictionary<CSharpCompilation, CSharpCompilation>();
        var reshaped = new HashSet<CSharpCompilation>();
        var trees = new Dictionary<string, SyntaxTree>(StringComparer.OrdinalIgnoreCase);
        foreach (var path in known.Keys.Where(Changed).ToList())
        {
            var (old, compilation) = current[path];
            var tree = CSharpSyntaxTree.ParseText(Texts[path], Parse, old.FilePath);
            if (Reshaped(path, tree))
            {
                reshaped.Add(compilation);
            }
            replaced[compilation] = replaced.GetValueOrDefault(compilation, compilation).ReplaceSyntaxTree(old, tree);
            trees[path] = tree;
        }
        // The next round starts from the files as they are now.
        foreach (var (path, tree) in trees)
        {
            current[path] = (tree, replaced[current[path].Compilation]);
        }
        return (replaced, reshaped, trees.Keys.ToHashSet(StringComparer.OrdinalIgnoreCase));
    }

    /// <summary>Whether a file's text is not what the last round read; it is remembered.</summary>
    static bool Changed(string path)
    {
        var text = File.ReadAllText(path);
        if (Texts.TryGetValue(path, out var last) && last == text)
        {
            return false;
        }
        Texts[path] = text;
        return true;
    }

    /// <summary>Whether a file's code outside its members' bodies is not what it was; it is remembered.</summary>
    static bool Reshaped(string path, SyntaxTree tree)
    {
        var shape = Shape(tree.GetRoot());
        var same = Shapes.TryGetValue(path, out var before) && before == shape;
        Shapes[path] = shape;
        return !same;
    }

    /// <summary>Every file of a project points at the project's newest compilation.</summary>
    static void Retarget(Dictionary<string, (SyntaxTree Tree, CSharpCompilation Compilation)> current, Dictionary<CSharpCompilation, CSharpCompilation> replaced)
    {
        foreach (var path in current.Keys.ToList())
        {
            var (tree, compilation) = current[path];
            if (replaced.TryGetValue(compilation, out var newer))
            {
                current[path] = (tree, newer);
            }
        }
    }

    static IEnumerable<(string Key, string Line)> Errors(CSharpCompilation compilation, SyntaxTree tree)
    {
        var root = tree.GetRoot();
        var found = compilation.GetSemanticModel(tree).GetDiagnostics()
            .Where(d => d.Severity == DiagnosticSeverity.Error && d.Location.IsInSource)
            .Select(d => (Id: d.Id, Message: d.GetMessage(), At: d.Location.GetMappedLineSpan().StartLinePosition,
                Node: root.FindNode(d.Location.SourceSpan, getInnermostNodeForTie: true)))
            .ToList();
        return found.Select(d =>
        {
            var key = Member(d.Node) + "\u0000" + d.Id + "\u0000" + d.Message;
            // The same error in the original code next to the mutant's is not the mutant's doing.
            var mutant = Arm(d.Node, null) is { } arm
                && !found.Any(o => o.Id == d.Id && o.Message == d.Message && arm.Original.Span.Contains(o.Node.Span) && Arm(o.Node, arm.Original) is null)
                ? $" [dgf mutant {arm.Id}]" : "";
            return (key, $"{tree.FilePath}({d.At.Line + 1},{d.At.Character + 1}): error {d.Id}: {d.Message}{mutant}");
        }).ToList();
    }

    /// <summary>The mutant whose own code a node is in, looking no further up than <paramref name="within"/>:
    /// the mutated arm of the innermost switch around it (<c>(Is(id) ? (mutated) : (original))</c>, or
    /// <c>if (Is(id)) {mutated} else {original}</c>), or its copy in a probe
    /// (<c>Check((original), id, (copy), ...)</c>). With it, the code that holds the original.</summary>
    static (int Id, SyntaxNode Original)? Arm(SyntaxNode node, SyntaxNode? within)
    {
        for (var child = node; child != within && child.Parent is { } parent; child = parent)
        {
            if (ArmOf(parent, child) is { } arm)
            {
                return arm;
            }
        }
        return null;
    }

    /// <summary>The mutant whose own code <paramref name="child"/> is, as a part of <paramref name="parent"/>.</summary>
    static (int Id, SyntaxNode Original)? ArmOf(SyntaxNode parent, SyntaxNode child) => parent switch
    {
        ConditionalExpressionSyntax choice when child == choice.WhenTrue => Of(Switch(choice.Condition), choice.WhenFalse),
        IfStatementSyntax { Else: { } other } choice when child == choice.Statement => Of(Switch(choice.Condition), other),
        ArgumentListSyntax list when child is ArgumentSyntax copy => Probed(list, list.Arguments.IndexOf(copy)),
        _ => null,
    };

    static (int Id, SyntaxNode Original)? Of(int? id, SyntaxNode original) => id is { } found ? (found, original) : null;

    /// <summary>The mutant whose copy is argument <paramref name="index"/> of a probe: <c>Check((original), id, (copy), ...)</c>.</summary>
    static (int Id, SyntaxNode Original)? Probed(ArgumentListSyntax list, int index)
    {
        var copy = index >= 2 && index % 2 == 0 && list.Parent is InvocationExpressionSyntax call && Named(call, "Check");
        return copy ? Of(list.Arguments[index - 1].Expression is LiteralExpressionSyntax { Token.Value: int id } ? id : null, list.Arguments[0]) : null;
    }

    /// <summary>The mutant a switch's condition asks for: <c>DgfMutants.Is(id)</c>.</summary>
    static int? Switch(ExpressionSyntax condition) =>
        condition is InvocationExpressionSyntax { ArgumentList.Arguments: [{ Expression: LiteralExpressionSyntax { Token.Value: int id } }] } call
            && Named(call, "Is") ? id : null;

    static bool Named(InvocationExpressionSyntax call, string method) =>
        call.Expression is MemberAccessExpressionSyntax { Name.Identifier.Text: var name, Expression: var on }
            && name == method && on.ToString().EndsWith("DgfMutants", StringComparison.Ordinal);

    /// <summary>A file's code outside its members' bodies: what other files can see of it.</summary>
    static string Shape(SyntaxNode root)
    {
        var text = new System.Text.StringBuilder();
        var position = root.FullSpan.Start;
        var source = root.SyntaxTree.GetText();
        foreach (var body in root.DescendantNodes(n => !IsBody(n)).Where(IsBody))
        {
            text.Append(source.ToString(Microsoft.CodeAnalysis.Text.TextSpan.FromBounds(position, body.SpanStart)));
            position = body.Span.End;
        }
        return text.Append(source.ToString(Microsoft.CodeAnalysis.Text.TextSpan.FromBounds(position, root.FullSpan.End))).ToString();
    }

    /// <summary>The body of a method, constructor, operator or accessor, or of an expression-bodied property.</summary>
    static bool IsBody(SyntaxNode node) => node is BlockSyntax or ArrowExpressionClauseSyntax && HasBody(node.Parent);

    static bool HasBody(SyntaxNode? member) =>
        member is BaseMethodDeclarationSyntax or AccessorDeclarationSyntax or PropertyDeclarationSyntax or IndexerDeclarationSyntax;

    /// <summary>The member a node is in, by its type and name and parameter count.</summary>
    static string Member(SyntaxNode node)
    {
        var member = node.AncestorsAndSelf().OfType<MemberDeclarationSyntax>().FirstOrDefault(m => m is not BaseTypeDeclarationSyntax);
        var types = node.Ancestors().OfType<BaseTypeDeclarationSyntax>().Select(t => t.Identifier.Text).Reverse();
        return string.Join(".", types) + ":" + (member is null ? "" : NameOf(member));
    }

    /// <summary>A member's name; methods and the like with their parameter count, since overloads share one.</summary>
    static string NameOf(MemberDeclarationSyntax member) => member switch
    {
        MethodDeclarationSyntax m => $"{m.Identifier.Text}/{m.ParameterList.Parameters.Count}",
        BaseMethodDeclarationSyntax b => $"{b.Kind()}/{b.ParameterList.Parameters.Count}",
        PropertyDeclarationSyntax p => p.Identifier.Text,
        FieldDeclarationSyntax f => string.Join(",", f.Declaration.Variables.Select(v => v.Identifier.Text)),
        _ => member.Kind().ToString(),
    };
}
