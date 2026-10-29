// dgf-dup-cs: C# methods that do the same thing, written differently.
//
//   dgf-dup-cs [paths...] [--root DIR] [--tests] [-o FILE] [--explain] [--timings] [--errors] [--confirm]
//   dgf-dup-cs [paths...] --compare Type.Method Type.Method
//
// With --regions, after the report it reads [{file, startLine, endLine}] on
// stdin and writes, for each, what the lines take and give if extracted
// (Regions.cs): dgf-dup sends its syntactic C# copies this way.
//
// Each project is compiled as it builds (its SDK's implicit usings, its
// packages, the projects it references); --timings prints how long each
// stage took and --errors the compile errors that leave methods out.
// --confirm runs each group's methods on generated inputs (see Confirm.cs).
//
// Writes dgf-dup's JSON report: one class per group of methods whose
// canonical expressions are equal (type 4, evidence "normalized"), with the
// rules that made them equal and the conditions those rules need.

using System.Collections.Concurrent;
using System.Text.Json;
using System.Text.Json.Serialization;
using DgfDup;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using Microsoft.CodeAnalysis.Operations;

Console.OutputEncoding = System.Text.Encoding.UTF8;
var options = Options.Parse(args);
var clock = System.Diagnostics.Stopwatch.StartNew();
var files = Sources.Find(options.Paths, options.Tests);
var workspace = Workspace.Load(files);
Timings.Lap(options, clock, $"parse {files.Count} files in {workspace.Compilations.Count()} projects");
var models = new ConcurrentDictionary<SyntaxTree, SemanticModel>();
// One method at a time, the biggest first: a few big files no longer keep one thread busy to the end.
var compilationOf = workspace.Scanned.ToDictionary(s => s.Tree, s => s.Compilation);
var work = workspace.Scanned
    .SelectMany(s => s.Tree.GetRoot().DescendantNodes().OfType<MethodDeclarationSyntax>())
    .OrderByDescending(m => m.Span.Length)
    .ToList();
if (options.Compare is [var left, var right])
{
    return Comparing.Run(workspace, work, left, right, options.Root);
}
var steps = QueryNames.Of(workspace.Compilations);
var methods = new ConcurrentBag<Reduced>();
var queries = new ConcurrentBag<QueryUse>();
Parallel.ForEach(System.Collections.Concurrent.Partitioner.Create(work, loadBalance: true), method =>
{
    // One model per file, shared by its methods: what one binds, the next reuses.
    var model = models.GetOrAdd(method.SyntaxTree, t => compilationOf[t].GetSemanticModel(t));
    if (Reduce.Method(method, model, options.Root, options.Explain) is { } reduced)
    {
        methods.Add(reduced);
    }
    foreach (var use in Reduce.Queries(method, model, options.Root, steps))
    {
        queries.Add(use);
    }
});
Timings.Lap(options, clock, $"reduce {work.Count} methods ({methods.Count} reduced, {queries.Count} queries)");
var confirm = options.Confirm ? new Confirm(workspace) : null;
var report = Grouping.Report(methods, confirm, QueryGroups.Of(queries.OrderBy(q => q.File, StringComparer.Ordinal).ThenBy(q => q.StartLine).ToList(), options.Root), files.Count);
Timings.Lap(options, clock, options.Confirm ? "group and confirm" : "group");
if (options.Errors)
{
    Sources.Errors(workspace);
}
var json = JsonSerializer.Serialize(report, Grouping.Json);
if (options.Output is { } output)
{
    File.WriteAllText(output, json);
    Console.Error.WriteLine($"Wrote the report to {output}");
}
else
{
    Console.WriteLine(json);
}
if (options.Regions)
{
    // dgf-dup's copies, sent once its own scan is done: what each would take and give as a method.
    Console.Out.Flush();
    Regions.Answer(workspace, options.Root);
}
return 0;

sealed record Options(List<string> Paths, string Root, bool Tests, string? Output, bool Explain, bool Timings, bool Errors, bool Confirm, string[]? Compare, bool Regions)
{
    public static Options Parse(string[] args)
    {
        var list = args.ToList();
        string? Take(string flag)
        {
            var at = list.IndexOf(flag);
            if (at < 0 || at + 1 >= list.Count)
            {
                return null;
            }
            var value = list[at + 1];
            list.RemoveRange(at, 2);
            return value;
        }
        var root = Take("--root") ?? Directory.GetCurrentDirectory();
        var output = Take("-o") ?? Take("--output");
        var tests = list.Remove("--tests");
        var explain = list.Remove("--explain");
        var timings = list.Remove("--timings");
        var errors = list.Remove("--errors");
        var confirm = list.Remove("--confirm");
        var regions = list.Remove("--regions");
        var compare = TakeTwo(list, "--compare");
        return new Options(list.Count == 0 ? ["."] : list, Path.GetFullPath(root), tests, output, explain, timings, errors, confirm, compare, regions);
    }

    /// <summary>A flag's two values, taken out of the list; null when it is not there.</summary>
    static string[]? TakeTwo(List<string> list, string flag)
    {
        var at = list.IndexOf(flag);
        if (at < 0 || at + 2 >= list.Count)
        {
            return null;
        }
        string[] values = [list[at + 1], list[at + 2]];
        list.RemoveRange(at, 3);
        return values;
    }
}

/// <summary>
/// <c>--compare Type.A Type.B</c>: two methods run on the same generated
/// inputs, as --confirm runs a group. Prints the verdict as JSON; exits 0
/// when they agree, 1 when an input tells them apart, 2 when they were not run.
/// </summary>
static class Comparing
{
    public static int Run(Workspace workspace, List<MethodDeclarationSyntax> methods, string left, string right, string root)
    {
        var compilationOf = workspace.Scanned.ToDictionary(s => s.Tree, s => s.Compilation);
        (IMethodSymbol, Node?)? Find(string name)
        {
            foreach (var method in methods)
            {
                var model = compilationOf[method.SyntaxTree].GetSemanticModel(method.SyntaxTree);
                if (model.GetDeclaredSymbol(method) is { } symbol && $"{symbol.ContainingType.Name}.{symbol.Name}" == name)
                {
                    return (symbol, Reduce.Method(method, model, root, explain: false, small: true)?.Expression);
                }
            }
            return null;
        }
        var verdict = (Find(left), Find(right)) switch
        {
            (null, _) => Confirmation.NotRun($"no method {left}"),
            (_, null) => Confirmation.NotRun($"no method {right}"),
            ({ } a, { } b) => new Confirm(workspace).Pair(a, b),
        };
        Console.WriteLine(JsonSerializer.Serialize(verdict, Grouping.Json));
        return verdict.Status switch { "confirmed" => 0, "disproved" => 1, _ => 2 };
    }
}

/// <summary>With <c>--timings</c>: how long each stage took, on stderr.</summary>
static class Timings
{
    public static void Lap(Options options, System.Diagnostics.Stopwatch clock, string stage)
    {
        if (options.Timings)
        {
            Console.Error.WriteLine($"{clock.Elapsed.TotalSeconds,6:F1} s  {stage}");
        }
    }
}

/// <summary>The names of LINQ's and EF Core's query calls: a method calling none of them has no query to read.</summary>
static class QueryNames
{
    public static HashSet<string> Of(IEnumerable<Compilation> compilations) =>
        compilations.SelectMany(c => new[] { "System.Linq.Queryable", "System.Linq.Enumerable", "Microsoft.EntityFrameworkCore.EntityFrameworkQueryableExtensions" }
            .Select(c.GetTypeByMetadataName))
            .OfType<INamedTypeSymbol>()
            .SelectMany(t => t.GetMembers().OfType<IMethodSymbol>().Select(m => m.Name))
            .ToHashSet(StringComparer.Ordinal);

    /// <summary>Whether the method calls something named like a query step.</summary>
    public static bool Calls(MethodDeclarationSyntax method, HashSet<string> names) =>
        method.DescendantNodes().OfType<InvocationExpressionSyntax>().Any(call => call.Expression switch
        {
            MemberAccessExpressionSyntax member => names.Contains(member.Name.Identifier.Text),
            MemberBindingExpressionSyntax binding => names.Contains(binding.Name.Identifier.Text),
            _ => false,
        });
}

static class Sources
{
    /// <summary>C# files under the paths: not build output, generated code or migrations, and not tests unless asked.</summary>
    public static List<string> Find(IEnumerable<string> paths, bool tests)
    {
        var found = new SortedSet<string>(StringComparer.Ordinal);
        foreach (var path in paths.Select(Path.GetFullPath))
        {
            var files = File.Exists(path) ? [path] : Directory.EnumerateFiles(path, "*.cs", SearchOption.AllDirectories);
            found.UnionWith(files.Where(f => !Skipped(f) && (tests || !IsTest(f)) && !Generated(f)));
        }
        return found.ToList();
    }

    /// <summary>A file a tool wrote, as its first lines say: <c>&lt;auto-generated&gt;</c>, "DO NOT EDIT", <c>@generated</c>.</summary>
    static bool Generated(string file) =>
        File.ReadLines(file).Take(10).Select(l => l.TrimStart())
            .Any(l => l.StartsWith("//", StringComparison.Ordinal)
                && (l.Contains("<auto-generated", StringComparison.Ordinal) || l.Contains("@generated", StringComparison.Ordinal)
                    || (l.Contains("generated", StringComparison.Ordinal) && l.Contains("DO NOT EDIT", StringComparison.Ordinal))));

    static bool Skipped(string file)
    {
        var parts = file.Split(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        var name = parts[^1].ToLowerInvariant();
        return parts.Any(p => p is "bin" or "obj" or ".git" or "node_modules" or "Migrations")
            || name.EndsWith(".g.cs") || name.EndsWith(".designer.cs");
    }

    static bool IsTest(string file)
    {
        var parts = file.Split(Path.DirectorySeparatorChar, Path.AltDirectorySeparatorChar);
        return parts[^1].EndsWith("Tests.cs") || parts[^1].EndsWith("Test.cs")
            || parts.Any(p => p.ToLowerInvariant() is "tests" or "test" or "e2e"
                || p.EndsWith("Tests", StringComparison.OrdinalIgnoreCase) && p.Contains('.'));
    }

    /// <summary>
    /// The compile errors in the scanned files, most common first: a method
    /// with one in it is bound wrong or left out, so this is where to look
    /// when fewer methods reduce than expected (a package not restored, say).
    /// </summary>
    public static void Errors(Workspace workspace)
    {
        var errors = workspace.Scanned.AsParallel()
            .SelectMany(s => s.Compilation.GetSemanticModel(s.Tree).GetDiagnostics().Where(d => d.Severity == DiagnosticSeverity.Error))
            .ToList();
        Console.Error.WriteLine($"{errors.Count} compile errors in the scanned files");
        foreach (var group in errors.GroupBy(d => d.Id).OrderByDescending(g => g.Count()).Take(10))
        {
            var first = group.First();
            Console.Error.WriteLine($"{group.Count(),6} {group.Key} {first.GetMessage()} ({Path.GetFileName(first.Location.SourceTree?.FilePath)}:{first.Location.GetLineSpan().StartLinePosition.Line + 1})");
        }
    }

    /// <summary>The C# files under a project's folder, not build output or generated code.</summary>
    public static IEnumerable<string> Under(string dir) =>
        Directory.EnumerateFiles(dir, "*.cs", SearchOption.AllDirectories).Where(f => !Skipped(f));
}

/// <summary>A method reduced to its canonical expression.</summary>
sealed record Reduced(string File, int StartLine, int EndLine, string Function, Node Expression, Trace Trace, IMethodSymbol Symbol);

static class Reduce
{
    /// <summary>Fewest nodes in an expression worth matching: smaller ones are one-liners anybody writes.</summary>
    const int MinSize = 10;

    /// <summary>A method's canonical expression; with <paramref name="small"/>, also one too small to match.</summary>
    public static Reduced? Method(MethodDeclarationSyntax syntax, SemanticModel model, string root, bool explain, bool small = false)
    {
        if (Start(syntax, model, explain) is not var (symbol, block))
        {
            return null;
        }
        var name = $"{symbol.ContainingType.Name}.{symbol.Name}";
        var trace = Canon.Begin();
        try
        {
            var expression = Canon.Reduce(Exec.Body(block, new Scope { Parameters = symbol.Parameters }), System.Collections.Immutable.ImmutableDictionary<string, bool>.Empty);
            Explain(explain, name, expression.Pretty());
            if (!small && !WorthMatching(expression))
            {
                return null;
            }
            var lines = syntax.GetLocation().GetLineSpan();
            var file = Path.GetRelativePath(root, syntax.SyntaxTree.FilePath).Replace('\\', '/');
            return new Reduced(file, lines.StartLinePosition.Line + 1, lines.EndLinePosition.Line + 1, name, expression, trace, symbol);
        }
        catch (Exception why) when (Expected.Contains(why.GetType()))
        {
            Explain(explain, name, "left out: " + why.Message);
            return null;
        }
    }

    /// <summary>A method's symbol and body, unless its syntax already shows it is left out.</summary>
    static (IMethodSymbol Symbol, IBlockOperation Block)? Start(MethodDeclarationSyntax syntax, SemanticModel model, bool explain)
    {
        if (Precheck.Why(syntax) is { } skipped)
        {
            Explain(explain, $"{(syntax.Parent as BaseTypeDeclarationSyntax)?.Identifier.Text}.{syntax.Identifier.Text}", "left out: " + skipped);
            return null;
        }
        return model.GetDeclaredSymbol(syntax) is { } symbol && Body(syntax, symbol, model) is { } block ? (symbol, block) : null;
    }

    /// <summary>The queries on tables in a method, wherever they are in it.</summary>
    public static IEnumerable<QueryUse> Queries(MethodDeclarationSyntax syntax, SemanticModel model, string root, HashSet<string> steps)
    {
        if (!QueryNames.Calls(syntax, steps) || model.GetDeclaredSymbol(syntax) is not { } symbol || model.GetOperation(syntax) is not { } body)
        {
            return [];
        }
        var file = Path.GetRelativePath(root, syntax.SyntaxTree.FilePath).Replace('\\', '/');
        return DgfDup.Queries.In(body, file, $"{symbol.ContainingType.Name}.{symbol.Name}").ToList();
    }

    /// <summary>A method's body when it returns a value.</summary>
    static IBlockOperation? Body(MethodDeclarationSyntax syntax, IMethodSymbol symbol, SemanticModel model) =>
        !symbol.ReturnsVoid && model.GetOperation(syntax) is IMethodBodyOperation body ? body.BlockBody ?? body.ExpressionBody : null;

    static bool WorthMatching(Node expression) =>
        expression.Size >= MinSize && !expression.Key.Contains("acc:") && !expression.Key.Contains("next(");

    static void Explain(bool explain, string name, string what)
    {
        if (explain)
        {
            Console.Error.WriteLine($"{name}: {what}");
        }
    }

    /// <summary>What code the engine does not model ends in: such a method is left out, not the scan.</summary>
    static readonly HashSet<Type> Expected =
    [
        typeof(UnsupportedException), typeof(InvalidCastException), typeof(IndexOutOfRangeException),
        typeof(InvalidOperationException), typeof(ArgumentException), typeof(NullReferenceException),
    ];
}

static class Grouping
{
    public static readonly JsonSerializerOptions Json = new()
    {
        WriteIndented = true,
        PropertyNamingPolicy = JsonNamingPolicy.CamelCase,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
    };

    /// <summary>
    /// The report: groups of methods with equal expressions, and the query
    /// groups. With <paramref name="confirm"/>, each group is run on
    /// generated inputs; one an input disproves is left out and listed.
    /// </summary>
    public static object Report(IEnumerable<Reduced> methods, Confirm? confirm, IEnumerable<Class> queries, int files)
    {
        var members = methods
            .GroupBy(m => m.Expression.Key)
            .Where(g => g.Count() > 1)
            .Select(g => g.OrderBy(m => m.File, StringComparer.Ordinal).ThenBy(m => m.StartLine).ToList())
            .ToList();
        confirm?.Prepare(members.Select(g => g.Select(m => (m.Symbol, m.Expression)).ToList()));
        var groups = members.Select(group => (Members: group, Confirmation: confirm?.Group([.. group.Select(m => (m.Symbol, m.Expression))])))
            .ToList();
        var disproved = groups.Where(g => g.Confirmation?.Status == "disproved")
            .Select(g => new { copies = g.Members.Select(m => new Copy(m.File, m.StartLine, m.EndLine, m.Function)).ToList(), example = g.Confirmation!.Example })
            .ToList();
        foreach (var group in disproved)
        {
            Console.Error.WriteLine($"disproved by running: {string.Join(" vs ", group.copies.Select(c => c.Function))}: {group.example}");
        }
        var classes = groups.Where(g => g.Confirmation?.Status != "disproved")
            .Select(g => Class(g.Members, g.Confirmation))
            .Concat(queries)
            .OrderByDescending(c => c.Saves)
            .ToList();
        var fileLines = classes.SelectMany(c => c.Copies).Select(c => c.File).Distinct()
            .ToDictionary(f => f, f => 0);
        return new
        {
            files,
            lines = 0,
            duplicatedLines = classes.Sum(c => c.Copies.Skip(1).Sum(x => x.EndLine + 1 - x.StartLine)),
            classes,
            fileLines,
            disproved,
        };
    }

    static Class Class(List<Reduced> members, Confirmation? confirmation)
    {
        var shortest = members.MinBy(m => m.EndLine - m.StartLine)!;
        var lines = members.Select(m => m.EndLine + 1 - m.StartLine).ToList();
        var rules = members.SelectMany(m => m.Trace.Rules).Distinct().Order(StringComparer.Ordinal).ToList();
        var conditions = members.SelectMany(m => m.Trace.Conditions).Distinct().Order(StringComparer.Ordinal).ToList();
        var others = members.Count - 1;
        return new Class
        {
            Copies = members.Select(m => new Copy(m.File, m.StartLine, m.EndLine, m.Function)).ToList(),
            Lines = lines.Max(),
            Statements = 0,
            Template = members[0].Expression.Pretty(),
            Saves = lines.Sum() - (shortest.EndLine + 1 - shortest.StartLine) - others,
            Unite = $"these {members.Count} methods compute the same result, written differently: keep one (the shortest is {shortest.Function}, {shortest.EndLine + 1 - shortest.StartLine} lines) and have the others call it, or remove them",
            Cautions = conditions.Select(c => "equal when " + c).ToList(),
            Rules = rules,
            Confirmation = confirmation,
        };
    }
}

public sealed record Copy(string File, int StartLine, int EndLine, string Function, string? Code = null);

public sealed class Class
{
    public string Kind { get; init; } = "copies";
    [JsonPropertyName("type")] public int CloneType { get; init; } = 4;
    public string Evidence { get; init; } = "normalized";
    public required List<Copy> Copies { get; init; }
    public int Lines { get; init; }
    public int Tokens { get; init; }
    public int Statements { get; init; }
    public required string Template { get; init; }
    public List<object> Holes { get; init; } = [];
    public int Saves { get; init; }
    public required string Unite { get; init; }
    public required List<string> Cautions { get; init; }
    /// <summary>The rules that made the methods' expressions equal.</summary>
    public required List<string> Rules { get; init; }
    /// <summary>With --confirm: what running the methods on the same inputs showed.</summary>
    public Confirmation? Confirmation { get; init; }
    /// <summary>For a repeated query: the entity (table) it reads.</summary>
    public string? Entity { get; init; }
}
