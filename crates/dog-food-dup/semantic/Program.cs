// dgf-dup-cs: C# methods that do the same thing, written differently.
//
//   dgf-dup-cs [paths...] [--root DIR] [--tests] [-o FILE]
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
var files = Sources.Find(options.Paths, options.Tests);
var compilation = Sources.Compile(files);
var methods = new ConcurrentBag<Reduced>();
Parallel.ForEach(compilation.SyntaxTrees.Where(t => t.FilePath.Length > 0), tree =>
{
    var model = compilation.GetSemanticModel(tree);
    foreach (var method in tree.GetRoot().DescendantNodes().OfType<MethodDeclarationSyntax>())
    {
        if (Reduce.Method(method, model, options.Root, options.Explain) is { } reduced)
        {
            methods.Add(reduced);
        }
    }
});
var report = Grouping.Report(methods, files.Count);
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

sealed record Options(List<string> Paths, string Root, bool Tests, string? Output, bool Explain)
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
        return new Options(list.Count == 0 ? ["."] : list, Path.GetFullPath(root), tests, output, explain);
    }
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
            found.UnionWith(files.Where(f => !Skipped(f) && (tests || !IsTest(f))));
        }
        return found.ToList();
    }

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
            || parts.Any(p => p.ToLowerInvariant() is "tests" or "test" or "e2e" || p.EndsWith(".Tests"));
    }

    /// <summary>
    /// One compilation of all the files against the running .NET's assemblies,
    /// with the usings SDK projects add: the framework's types resolve exactly,
    /// which is what the rules need; packages the code uses do not, and
    /// methods that need them are left out.
    /// </summary>
    public static CSharpCompilation Compile(List<string> files)
    {
        var parse = new CSharpParseOptions(LanguageVersion.Preview);
        var trees = files.AsParallel().AsOrdered()
            .Select(f => CSharpSyntaxTree.ParseText(File.ReadAllText(f), parse, f))
            .ToList();
        trees.Add(CSharpSyntaxTree.ParseText(ImplicitUsings, parse));
        var references = References(files).Select(p => MetadataReference.CreateFromFile(p));
        return CSharpCompilation.Create("scanned", trees, references,
            new CSharpCompilationOptions(OutputKind.DynamicallyLinkedLibrary, nullableContextOptions: NullableContextOptions.Enable));
    }

    /// <summary>
    /// The running .NET's assemblies, the packages restored projects use (from
    /// their obj/project.assets.json) and ASP.NET Core's shared framework when
    /// a project references it: one assembly per file name, the framework's first.
    /// </summary>
    static IEnumerable<string> References(List<string> files)
    {
        var byName = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        void Add(string path) => byName.TryAdd(Path.GetFileName(path), path);
        foreach (var path in ((string)AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES")!).Split(Path.PathSeparator))
        {
            Add(path);
        }
        var assets = files.Select(Path.GetDirectoryName).Distinct()
            .SelectMany(dir => Ancestors(dir!).Select(d => Path.Combine(d, "obj", "project.assets.json")))
            .Distinct().Where(File.Exists).ToList();
        var aspNet = false;
        foreach (var file in assets)
        {
            aspNet |= Packages(file, Add);
        }
        if (aspNet && SharedFramework("Microsoft.AspNetCore.App") is { } shared)
        {
            foreach (var dll in Directory.EnumerateFiles(shared, "*.dll"))
            {
                Add(dll);
            }
        }
        return byName.Values;
    }

    static IEnumerable<string> Ancestors(string dir)
    {
        for (var at = dir; at is not null; at = Path.GetDirectoryName(at))
        {
            yield return at;
        }
    }

    /// <summary>Adds the compile assemblies of a restored project's packages; whether it uses ASP.NET Core.</summary>
    static bool Packages(string assetsFile, Action<string> add)
    {
        using var json = System.Text.Json.JsonDocument.Parse(File.ReadAllText(assetsFile));
        var root = json.RootElement;
        foreach (var dll in PackageAssemblies(root).Where(File.Exists))
        {
            add(dll);
        }
        return root.GetProperty("project").GetProperty("frameworks").EnumerateObject()
            .Any(f => f.Value.TryGetProperty("frameworkReferences", out var refs) && refs.TryGetProperty("Microsoft.AspNetCore.App", out _));
    }

    /// <summary>The compile assemblies of the first target's packages (projects are compiled from their sources).</summary>
    static IEnumerable<string> PackageAssemblies(System.Text.Json.JsonElement root)
    {
        var folder = root.GetProperty("packageFolders").EnumerateObject().First().Name;
        var libraries = root.GetProperty("libraries");
        var target = root.GetProperty("targets").EnumerateObject().First();
        return target.Value.EnumerateObject()
            .Where(p => p.Value.TryGetProperty("type", out var type) && type.GetString() == "package")
            .Where(p => p.Value.TryGetProperty("compile", out _) && libraries.TryGetProperty(p.Name, out _))
            .SelectMany(p => p.Value.GetProperty("compile").EnumerateObject()
                .Where(a => a.Name.EndsWith(".dll", StringComparison.OrdinalIgnoreCase))
                .Select(a => Path.Combine(folder, libraries.GetProperty(p.Name).GetProperty("path").GetString()!, a.Name)));
    }

    /// <summary>A shared framework of the running .NET's version, next to it.</summary>
    static string? SharedFramework(string name)
    {
        var runtime = System.Runtime.InteropServices.RuntimeEnvironment.GetRuntimeDirectory().TrimEnd(Path.DirectorySeparatorChar);
        var version = Path.GetFileName(runtime);
        var dir = Path.Combine(Path.GetDirectoryName(Path.GetDirectoryName(runtime)!)!, name, version);
        return Directory.Exists(dir) ? dir : null;
    }

    const string ImplicitUsings = """
        global using global::System;
        global using global::System.Collections.Generic;
        global using global::System.IO;
        global using global::System.Linq;
        global using global::System.Net.Http;
        global using global::System.Threading;
        global using global::System.Threading.Tasks;
        """;
}

/// <summary>A method reduced to its canonical expression.</summary>
sealed record Reduced(string File, int StartLine, int EndLine, string Function, Node Expression, Trace Trace);

static class Reduce
{
    /// <summary>Fewest nodes in an expression worth matching: smaller ones are one-liners anybody writes.</summary>
    const int MinSize = 10;

    public static Reduced? Method(MethodDeclarationSyntax syntax, SemanticModel model, string root, bool explain)
    {
        if (model.GetDeclaredSymbol(syntax) is not { } symbol || Body(syntax, symbol, model) is not { } block)
        {
            return null;
        }
        var name = $"{symbol.ContainingType.Name}.{symbol.Name}";
        var trace = Canon.Begin();
        try
        {
            var expression = Canon.Reduce(Exec.Body(block, new Scope { Parameters = symbol.Parameters }), System.Collections.Immutable.ImmutableDictionary<string, bool>.Empty);
            Explain(explain, name, expression.Pretty());
            if (!WorthMatching(expression))
            {
                return null;
            }
            var lines = syntax.GetLocation().GetLineSpan();
            var file = Path.GetRelativePath(root, syntax.SyntaxTree.FilePath).Replace('\\', '/');
            return new Reduced(file, lines.StartLinePosition.Line + 1, lines.EndLinePosition.Line + 1, name, expression, trace);
        }
        catch (Exception why) when (Expected.Contains(why.GetType()))
        {
            Explain(explain, name, "left out: " + why.Message);
            return null;
        }
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

    public static object Report(IEnumerable<Reduced> methods, int files)
    {
        var classes = methods
            .GroupBy(m => m.Expression.Key)
            .Where(g => g.Count() > 1)
            .Select(g => Class(g.OrderBy(m => m.File, StringComparer.Ordinal).ThenBy(m => m.StartLine).ToList()))
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
        };
    }

    static Class Class(List<Reduced> members)
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
        };
    }
}

sealed record Copy(string File, int StartLine, int EndLine, string Function);

sealed class Class
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
}
