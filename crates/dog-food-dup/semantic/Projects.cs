using System.Collections.Concurrent;
using System.Text.Json;
using System.Text.RegularExpressions;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.Diagnostics;
using System.Collections.Immutable;

namespace DgfDup;

/// <summary>
/// A project file as the compiler needs it: its SDK, global usings, packages
/// and the projects it references. Read from the XML, without MSBuild.
/// </summary>
sealed record Project(string File, string Name, string Sdk, bool Executable, bool ImplicitUsings, List<string> Usings, List<string> Removed, List<string> References)
{
    public string Dir => Path.GetDirectoryName(File)!;

    public static Project Read(string file)
    {
        var text = System.IO.File.ReadAllText(file);
        // Directory.Build.props sets these for every project under it.
        var props = Ancestors(Path.GetDirectoryName(file)!).Select(d => Path.Combine(d, "Directory.Build.props")).FirstOrDefault(System.IO.File.Exists);
        var all = props is null ? text : System.IO.File.ReadAllText(props) + text;
        var dir = Path.GetDirectoryName(file)!;
        return new Project(
            file,
            Property(text, "AssemblyName") ?? Path.GetFileNameWithoutExtension(file),
            Regex.Match(text, "Sdk=\"([^\"/]+)").Groups[1].Value,
            IsExecutable(text),
            Property(all, "ImplicitUsings") is "enable" or "true",
            Items(all, "Using", "Include"),
            Items(all, "Using", "Remove"),
            Items(text, "ProjectReference", "Include").Select(r => Path.GetFullPath(Path.Combine(dir, r.Replace('\\', Path.DirectorySeparatorChar)))).ToList());
    }

    /// <summary>An app, not a library: as its output type says, or as web and worker projects are by default.</summary>
    static bool IsExecutable(string text) =>
        Property(text, "OutputType") is { } output
            ? output is "Exe" or "WinExe"
            : Regex.IsMatch(text, @"Sdk=""Microsoft\.NET\.Sdk\.(Web|Worker)");

    static string? Property(string xml, string name) =>
        Regex.Matches(xml, $"<{name}>\\s*([^<]*?)\\s*</{name}>").LastOrDefault()?.Groups[1].Value;

    static List<string> Items(string xml, string item, string attribute) =>
        Regex.Matches(xml, $"<{item}\\s+[^>]*?\\b{attribute}=\"([^\"]+)\"").Select(m => m.Groups[1].Value).ToList();

    public static IEnumerable<string> Ancestors(string dir)
    {
        for (var at = dir; at is not null; at = Path.GetDirectoryName(at))
        {
            yield return at;
        }
    }

    /// <summary>The usings an SDK adds with <c>&lt;ImplicitUsings&gt;</c> on.</summary>
    public IEnumerable<string> Implicit()
    {
        if (!ImplicitUsings)
        {
            return [];
        }
        string[] hosting = ["Microsoft.Extensions.Configuration", "Microsoft.Extensions.DependencyInjection", "Microsoft.Extensions.Hosting", "Microsoft.Extensions.Logging"];
        return Sdk switch
        {
            "Microsoft.NET.Sdk.Web" => [.. Base, "System.Net.Http.Json", "Microsoft.AspNetCore.Builder", "Microsoft.AspNetCore.Hosting", "Microsoft.AspNetCore.Http", "Microsoft.AspNetCore.Routing", .. hosting],
            "Microsoft.NET.Sdk.Worker" => [.. Base, .. hosting],
            _ => Base,
        };
    }

    public static readonly string[] Base = ["System", "System.Collections.Generic", "System.IO", "System.Linq", "System.Net.Http", "System.Threading", "System.Threading.Tasks"];
}

/// <summary>
/// The scanned files compiled the way their projects build them: one
/// compilation per project, against its packages and the projects it
/// references (compiled from their sources too, whether scanned or not).
/// Files in no project are compiled together, against the running .NET.
/// </summary>
public sealed class Workspace
{
    /// <summary>Each scanned file's tree, with the compilation it belongs to.</summary>
    public required IReadOnlyList<(SyntaxTree Tree, CSharpCompilation Compilation)> Scanned { get; init; }

    public IEnumerable<CSharpCompilation> Compilations => Scanned.Select(s => s.Compilation).Distinct();

    /// <summary>Every compilation, with the referenced projects nothing scanned is in.</summary>
    public required IReadOnlyList<CSharpCompilation> All { get; init; }

    static readonly CSharpParseOptions Parse = new(LanguageVersion.Preview);

    public static Workspace Load(List<string> files)
    {
        var owner = new ConcurrentDictionary<string, string?>(StringComparer.OrdinalIgnoreCase);
        string? ProjectOf(string file) => owner.GetOrAdd(Path.GetDirectoryName(file)!, dir =>
            Project.Ancestors(dir).Select(d => Directory.Exists(d) ? Directory.EnumerateFiles(d, "*.csproj").FirstOrDefault() : null)
                .FirstOrDefault(p => p is not null));
        var projects = new Dictionary<string, Project>(StringComparer.OrdinalIgnoreCase);
        void Add(string file)
        {
            if (projects.ContainsKey(file) || !File.Exists(file))
            {
                return;
            }
            var project = Project.Read(file);
            projects[file] = project;
            project.References.ForEach(Add);
        }
        foreach (var file in files.Select(ProjectOf).OfType<string>().Distinct())
        {
            Add(file);
        }
        // A project builds every .cs file under it that no nested project owns.
        var sources = projects.Values.AsParallel()
            .SelectMany(p => Sources.Under(p.Dir).Where(f => string.Equals(ProjectOf(f), p.File, StringComparison.OrdinalIgnoreCase)).Select(f => (p.File, f))).ToList()
            .Concat(files.Where(f => ProjectOf(f) is null).Select(f => ("", f)))
            .ToLookup(x => x.Item1, x => x.Item2, StringComparer.OrdinalIgnoreCase);
        var trees = sources.SelectMany(g => g).Distinct(StringComparer.OrdinalIgnoreCase).AsParallel()
            .ToDictionary(f => f, f => CSharpSyntaxTree.ParseText(File.ReadAllText(f), Parse, f), StringComparer.OrdinalIgnoreCase);
        var compiled = new Dictionary<string, CSharpCompilation>(StringComparer.OrdinalIgnoreCase);
        CSharpCompilation Compile(Project project)
        {
            if (compiled.TryGetValue(project.File, out var done))
            {
                return done;
            }
            compiled[project.File] = null!; // a reference cycle stops here
            // Project references are transitive, as MSBuild makes them.
            var referenced = Closure(project).Select(r => Compile(projects[r])).Where(c => c is not null)
                .Select(c => (MetadataReference)c.ToMetadataReference()).ToList();
            var usings = project.Implicit().Concat(project.Usings).Except(project.Removed).Distinct();
            var own = sources[project.File].Select(f => trees[f]).Append(Usings(usings));
            var assets = Path.Combine(project.Dir, "obj", "project.assets.json");
            return compiled[project.File] = Create(project.Name, project.Executable, own, References.For(assets, UsesAspNet(project)).Concat(referenced));
        }
        IEnumerable<string> Closure(Project project)
        {
            var seen = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
            var next = new Stack<string>(project.References);
            while (next.TryPop(out var at))
            {
                if (projects.TryGetValue(at, out var found) && !string.Equals(at, project.File, StringComparison.OrdinalIgnoreCase) && seen.Add(at))
                {
                    found.References.ForEach(next.Push);
                }
            }
            return seen;
        }
        // ASP.NET Core's framework comes with the projects referenced: a test project of a web API has it too.
        var aspNet = new Dictionary<string, bool>(StringComparer.OrdinalIgnoreCase);
        bool UsesAspNet(Project project)
        {
            if (aspNet.TryGetValue(project.File, out var known))
            {
                return known;
            }
            aspNet[project.File] = false;
            return aspNet[project.File] = project.Sdk == "Microsoft.NET.Sdk.Web" || References.FrameworkReference(Path.Combine(project.Dir, "obj", "project.assets.json"))
                || Closure(project).Any(r => UsesAspNet(projects[r]));
        }
        var loose = sources[""].ToList();
        CSharpCompilation? scattered = loose.Count == 0 ? null : Create("scanned", false, loose.Select(f => trees[f]).Append(Usings(Project.Base)),
            References.For(loose.Select(Path.GetDirectoryName).Distinct()
                .SelectMany(d => Project.Ancestors(d!)).Select(d => Path.Combine(d, "obj", "project.assets.json")).FirstOrDefault(File.Exists), false));
        var scanned = files.Select(f => (trees[f], ProjectOf(f) is { } p ? Compile(projects[p]) : scattered!)).ToList();
        return new Workspace { Scanned = scanned, All = [.. compiled.Values.Where(c => c is not null), .. scattered is null ? [] : new[] { scattered }] };
    }

    static SyntaxTree Usings(IEnumerable<string> namespaces) =>
        CSharpSyntaxTree.ParseText(string.Concat(namespaces.Select(n => $"global using global::{n};\n")), Parse);

    static CSharpCompilation Create(string name, bool executable, IEnumerable<SyntaxTree> trees, IEnumerable<MetadataReference> references) =>
        CSharpCompilation.Create(name, trees, references,
            new CSharpCompilationOptions(executable ? OutputKind.ConsoleApplication : OutputKind.DynamicallyLinkedLibrary, nullableContextOptions: NullableContextOptions.Enable));
}

/// <summary>
/// The assemblies a project compiles against: the running .NET's, its
/// packages (from its obj/project.assets.json) and ASP.NET Core's shared
/// framework when it uses it. Where two have one name, the newer wins: an
/// old package of an ASP.NET Core part loses to the framework's own.
/// </summary>
static class References
{
    static readonly ConcurrentDictionary<string, MetadataReference> Loaded = new(StringComparer.OrdinalIgnoreCase);
    static readonly ConcurrentDictionary<string, Version?> Versions = new(StringComparer.OrdinalIgnoreCase);

    public static IEnumerable<MetadataReference> For(string? assets, bool aspNet)
    {
        var byName = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        void Add(string path) => Newer(byName, path);
        foreach (var path in ((string)AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES")!).Split(Path.PathSeparator))
        {
            Add(path);
        }
        aspNet |= assets is not null && File.Exists(assets) && Packages(assets, Add);
        foreach (var dll in aspNet ? AspNetFramework() : [])
        {
            Add(dll);
        }
        return byName.Values.Select(p => Loaded.GetOrAdd(p, path => MetadataReference.CreateFromFile(path)));
    }

    /// <summary>Keeps the newer of two assemblies with one name; a native library (ASP.NET Core's IIS module) is no reference.</summary>
    static void Newer(Dictionary<string, string> byName, string path)
    {
        var name = Path.GetFileName(path);
        if (Version(path) is { } version && (!byName.TryGetValue(name, out var had) || version > Version(had)))
        {
            byName[name] = path;
        }
    }

    static IEnumerable<string> AspNetFramework() =>
        SharedFramework("Microsoft.AspNetCore.App") is { } shared ? Directory.EnumerateFiles(shared, "*.dll") : [];

    /// <summary>An assembly's version, or null when the file is not a .NET assembly.</summary>
    static Version? Version(string path) => Versions.GetOrAdd(path, p =>
    {
        try
        {
            return System.Reflection.AssemblyName.GetAssemblyName(p).Version ?? new Version();
        }
        catch (BadImageFormatException)
        {
            return null;
        }
    });

    /// <summary>Whether a restored project references ASP.NET Core's shared framework.</summary>
    public static bool FrameworkReference(string assetsFile)
    {
        if (!File.Exists(assetsFile))
        {
            return false;
        }
        using var json = JsonDocument.Parse(File.ReadAllText(assetsFile));
        return UsesAspNet(json.RootElement);
    }

    static bool UsesAspNet(JsonElement root) =>
        root.GetProperty("project").GetProperty("frameworks").EnumerateObject()
            .Any(f => f.Value.TryGetProperty("frameworkReferences", out var refs) && refs.TryGetProperty("Microsoft.AspNetCore.App", out _));

    /// <summary>Adds the compile assemblies of a restored project's packages; whether it uses ASP.NET Core.</summary>
    static bool Packages(string assetsFile, Action<string> add)
    {
        using var json = JsonDocument.Parse(File.ReadAllText(assetsFile));
        var root = json.RootElement;
        foreach (var dll in PackageAssemblies(root).Where(File.Exists))
        {
            add(dll);
        }
        return UsesAspNet(root);
    }

    /// <summary>The compile assemblies of the first target's packages (projects are compiled from their sources).</summary>
    static IEnumerable<string> PackageAssemblies(JsonElement root)
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
}

/// <summary>
/// The SDK's source generators, run before a project is compiled to run it:
/// a <c>[GeneratedRegex]</c> or <c>[LoggerMessage]</c> partial method has no
/// body until its generator writes one. Reading code does not need them, as
/// calls to such a method bind to its declaration.
/// </summary>
static class Generators
{
    /// <summary>The generators for the markers; the others (minimal API, configuration binding) are opt-in in a build too.</summary>
    static readonly HashSet<string> Wanted =
    [
        "System.Text.RegularExpressions.Generator", "System.Text.Json.SourceGeneration", "Microsoft.Interop.LibraryImportGenerator",
        "Microsoft.Extensions.Logging.Generators", "Microsoft.Extensions.Options.SourceGeneration",
    ];

    static readonly string[] Markers = ["[GeneratedRegex", "[LoggerMessage", "[JsonSerializable", "[LibraryImport", "[OptionsValidator"];

    static readonly Lazy<ImmutableArray<ISourceGenerator>> Core = new(() => From("Microsoft.NETCore.App.Ref"));
    static readonly Lazy<ImmutableArray<ISourceGenerator>> AspNet = new(() => From("Microsoft.AspNetCore.App.Ref"));

    public static CSharpCompilation Run(CSharpCompilation compilation)
    {
        if (!compilation.SyntaxTrees.Any(t => t.TryGetText(out var text) ? Needs(text.ToString()) : Needs(t.GetText().ToString())))
        {
            return compilation;
        }
        // The logging and options generators ship with ASP.NET Core's pack, but any project may use them.
        var generators = Core.Value.AddRange(AspNet.Value);
        CSharpGeneratorDriver.Create(generators, parseOptions: (CSharpParseOptions)compilation.SyntaxTrees.First().Options)
            .RunGeneratorsAndUpdateCompilation(compilation, out var updated, out _);
        return (CSharpCompilation)updated;
    }

    static bool Needs(string text) => Markers.Any(m => text.Contains(m, StringComparison.Ordinal));

    /// <summary>The generators a targeting pack ships, for the running .NET's version.</summary>
    static ImmutableArray<ISourceGenerator> From(string pack)
    {
        var runtime = System.Runtime.InteropServices.RuntimeEnvironment.GetRuntimeDirectory().TrimEnd(Path.DirectorySeparatorChar);
        var root = Path.GetDirectoryName(Path.GetDirectoryName(Path.GetDirectoryName(runtime)!)!)!;
        var dir = Path.Combine(root, "packs", pack, Path.GetFileName(runtime), "analyzers", "dotnet", "cs");
        if (!Directory.Exists(dir))
        {
            return [];
        }
        return [.. Directory.EnumerateFiles(dir, "*.dll")
            .Where(f => Wanted.Contains(Path.GetFileNameWithoutExtension(f)))
            .Select(f => new AnalyzerFileReference(f, Loader.Instance))
            .SelectMany(r => r.GetGenerators(LanguageNames.CSharp))];
    }

    sealed class Loader : IAnalyzerAssemblyLoader
    {
        public static readonly Loader Instance = new();

        public void AddDependencyLocation(string fullPath) { }

        public System.Reflection.Assembly LoadFromPath(string fullPath) => System.Reflection.Assembly.LoadFrom(fullPath);
    }
}
