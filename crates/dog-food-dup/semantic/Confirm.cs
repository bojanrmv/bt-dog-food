using System.Collections;
using System.Globalization;
using System.Reflection;
using System.Runtime.CompilerServices;
using System.Runtime.Loader;
using Microsoft.CodeAnalysis;
using Microsoft.CodeAnalysis.CSharp;
using Microsoft.CodeAnalysis.CSharp.Syntax;
using NullabilityInfo = System.Reflection.NullabilityInfo;

namespace DgfDup;

/// <summary>
/// What running a group's methods on the same inputs showed: <c>confirmed</c>
/// (they agreed on every input), <c>disproved</c> (an input on which they
/// differ: the engine was wrong) or <c>not-run</c> (why they could not be).
/// </summary>
public sealed record Confirmation(string Status, int Tries = 0, string? Example = null, string? Reason = null)
{
    public static Confirmation NotRun(string reason) => new("not-run", Reason: reason);
}

/// <summary>
/// With <c>--confirm</c>: a group of methods said to do the same thing is run
/// on generated inputs, and a counterexample disproves it. Only methods
/// without effects are run (their canonical expressions call none of the
/// project's own methods or constructors, no framework code that reaches
/// files, network or processes, and read no database), so running them
/// changes nothing. Property getters and static initializers do run. The projects are compiled in memory and loaded into their
/// own context; an instance method runs on an object made without its
/// constructor, with the same generated values in fields of the same name.
/// </summary>
public sealed class Confirm(Workspace workspace)
{
    /// <summary>Inputs tried per pair.</summary>
    public const int Tries = 1000;

    /// <summary>Fewest inputs that must return a value (not throw) for a group to count as confirmed.</summary>
    const int Returned = 10;

    static readonly TimeSpan Limit = TimeSpan.FromSeconds(2);

    readonly Loader loader = new(workspace);

    readonly Callees callees = new(workspace);

    /// <summary>Compiles the projects of the groups that will run, in parallel, before running any.</summary>
    public void Prepare(IEnumerable<IReadOnlyList<(IMethodSymbol Symbol, Node Expression)>> groups) =>
        loader.Prepare(groups.Where(g => g.All(m => Unsafe(m.Symbol, m.Expression) is null)).SelectMany(g => g.Select(m => m.Symbol)));

    public Confirmation Group(IReadOnlyList<(IMethodSymbol Symbol, Node Expression)> members)
    {
        if (members.Select(m => Unsafe(m.Symbol, m.Expression)).FirstOrDefault(r => r is not null) is { } why)
        {
            return Confirmation.NotRun(why);
        }
        var loaded = members.Select(m => loader.Method(m.Symbol)).ToList();
        if (loaded.FirstOrDefault(l => l.Method is null) is { Reason: { } missing })
        {
            return Confirmation.NotRun(missing);
        }
        return Agree([.. loaded.Select(l => l.Method!)], Constants.Of(members.Select(m => m.Expression)));
    }

    /// <summary>The first method against each of the others: the first verdict that is not a confirmation, else the last.</summary>
    Confirmation Agree(List<MethodInfo> methods, Constants constants)
    {
        var verdict = Confirmation.NotRun("one method");
        try
        {
            foreach (var other in methods.Skip(1))
            {
                verdict = Compare(methods[0], other, constants);
                if (verdict.Status != "confirmed")
                {
                    return verdict;
                }
            }
        }
        catch (Exception e) when (e is FileNotFoundException or FileLoadException or TypeLoadException)
        {
            return Confirmation.NotRun(loader.Missing(e));
        }
        return verdict;
    }

    /// <summary>Two methods, for <c>--compare</c>: run only when the engine could reduce both, and so knows they have no effects.</summary>
    public Confirmation Pair((IMethodSymbol Symbol, Node? Expression) a, (IMethodSymbol Symbol, Node? Expression) b)
    {
        if (a.Expression is null || b.Expression is null)
        {
            return Confirmation.NotRun($"the engine cannot reduce {(a.Expression is null ? a.Symbol.Name : b.Symbol.Name)}, so it cannot tell it has no effects");
        }
        return Group([(a.Symbol, a.Expression), (b.Symbol, b.Expression)]);
    }

    /// <summary>Why a method is not run: it could change something, or what it returns is not a plain value.</summary>
    string? Unsafe(IMethodSymbol symbol, Node expression) =>
        Generic(symbol) ? "a generic method"
        : Async(symbol) ? "an async method"
        : Database.Any(expression.Key.Contains) ? "it reads a database"
        : Effects(expression, 0);

    static bool Generic(IMethodSymbol symbol) => symbol.IsGenericMethod || symbol.ContainingType.IsGenericType;

    static bool Async(IMethodSymbol symbol) => symbol.IsAsync || symbol.ReturnType.Name is "Task" or "ValueTask";

    static readonly string[] Database = ["EntityFrameworkCore", "IQueryable", "DbSet"];

    /// <summary>Calls followed into the project's own methods, at most this deep.</summary>
    const int Deepest = 4;

    /// <summary>
    /// What in an expression could reach outside the process: an await; the
    /// framework's files, network, console, environment, processes and
    /// threads; a constructor of the project's own types; or a call to one of
    /// its methods that the engine cannot reduce or that has such effects.
    /// </summary>
    string? Effects(Node n, int depth) =>
        Own(n, depth) ?? n.Kids.Select(k => Effects(k, depth)).FirstOrDefault(r => r is not null);

    /// <summary>What the node itself does, not its children.</summary>
    string? Own(Node n, int depth) =>
        n.StartsWith("await") ? "an await" : Changes(n) ?? Made(n) ?? Call(n, depth);

    static string? Changes(Node n) =>
        Changing.FirstOrDefault(c => n.Head.Contains(c, StringComparison.Ordinal)) is { } changing
            ? $"it reads {changing}, which differs from one run to the next"
            : null;

    string? Made(Node n) =>
        n.StartsWith("new:") && !Harmless(n.Head[4..]) && !callees.PlainConstructor(n.Head[4..])
            ? $"it makes a {Short(n.Head[4..])}, whose constructor or initializers may have effects or read the clock"
            : null;

    string? Call(Node n, int depth) =>
        n.StartsWith("call:") && !Harmless(n.Head[5..]) ? Called(n.Head[5..], depth) : null;

    /// <summary>Why a call into the project's code may have effects, or null when its body has none.</summary>
    string? Called(string method, int depth)
    {
        if (depth >= Deepest)
        {
            return $"it calls {Short(method)} through too many methods to follow";
        }
        return callees.Expression(method) is { } body
            ? Effects(body, depth + 1)
            : $"it calls {Short(method)}, which may have effects";
    }

    /// <summary>Values that differ between two runs: two methods reading them can not be compared by running.</summary>
    static readonly string[] Changing =
    [
        "System.DateTime.Now", "System.DateTime.UtcNow", "System.DateTime.Today", "System.DateTimeOffset.Now",
        "System.DateTimeOffset.UtcNow", "System.Guid.NewGuid", "System.Environment.TickCount",
    ];

    static bool Harmless(string symbol) =>
        (symbol.StartsWith("System.", StringComparison.Ordinal) || char.IsLower(symbol[0]))
        && !Outside.Any(prefix => symbol.StartsWith(prefix, StringComparison.Ordinal));

    static readonly string[] Outside =
    [
        "System.IO.", "System.Net.", "System.Console.", "System.Environment.", "System.Diagnostics.", "System.Threading.",
        "System.Random.", "System.Runtime.", "System.Reflection.", "System.AppDomain.", "System.GC.", "System.Activator.",
    ];

    static string Short(string symbol)
    {
        var paren = symbol.IndexOf('(');
        return paren < 0 ? symbol : symbol[..paren];
    }

    /// <summary>Both methods on the same inputs, until they differ.</summary>
    static Confirmation Compare(MethodInfo a, MethodInfo b, Constants constants)
    {
        if (!a.GetParameters().Select(p => p.ParameterType).SequenceEqual(b.GetParameters().Select(p => p.ParameterType)))
        {
            return Confirmation.NotRun("other parameter types");
        }
        var tally = new Tally();
        for (var i = 0; i < Tries; i++)
        {
            if (Try(a, b, i, constants, tally) is { } settled)
            {
                return settled;
            }
        }
        return tally.Verdict();
    }

    /// <summary>One input: a verdict when it settles the comparison, else null.</summary>
    static Confirmation? Try(MethodInfo a, MethodInfo b, int seed, Constants constants, Tally tally)
    {
        Outcome left, right;
        try
        {
            left = Run(a, seed, constants);
            right = Run(b, seed, constants);
        }
        catch (Inputs.CannotMake cannot)
        {
            return Confirmation.NotRun(cannot.Message);
        }
        if (left.TimedOut || right.TimedOut)
        {
            return Confirmation.NotRun("it ran past the time limit");
        }
        if (left.Text != right.Text)
        {
            var shown = string.Join(", ", new Inputs(seed, constants).Arguments(a).Select(Show.Value));
            return new Confirmation("disproved", seed + 1, $"({shown}) gives {left.Text} vs {right.Text}");
        }
        tally.Add(left);
        return null;
    }

    /// <summary>The inputs both methods agreed on: enough of them must return, not throw.</summary>
    sealed class Tally
    {
        int returned;
        string? thrown;

        public void Add(Outcome outcome)
        {
            if (outcome.Threw)
            {
                thrown ??= outcome.Text;
                return;
            }
            returned++;
        }

        public Confirmation Verdict() => returned >= Returned
            ? new Confirmation("confirmed", Tries)
            : Confirmation.NotRun($"most inputs end in an exception ({thrown})");
    }

    sealed record Outcome(string Text, bool Threw, bool TimedOut);

    /// <summary>One run on the inputs of try <paramref name="seed"/>: made afresh, so neither method sees what the other did.</summary>
    static Outcome Run(MethodInfo method, int seed, Constants constants)
    {
        var inputs = new Inputs(seed, constants);
        var target = method.IsStatic ? null : inputs.Instance(method.DeclaringType!);
        var args = inputs.Arguments(method);
        var run = Task.Run(() =>
        {
            try
            {
                return new Outcome(Show.Value(method.Invoke(target, args)), false, false);
            }
            catch (TargetInvocationException e) when (e.InnerException is not null)
            {
                return new Outcome("throws " + e.InnerException.GetType().Name, true, false);
            }
        });
        return run.Wait(Limit) ? run.Result : new Outcome("", false, true);
    }

    /// <summary>
    /// Loads the scanned projects compiled in memory. Their packages and
    /// ASP.NET Core's framework load from where the compiler found them;
    /// .NET's own assemblies are shared with this process.
    /// </summary>
    sealed class Loader : AssemblyLoadContext
    {
        readonly Dictionary<string, CSharpCompilation> projects;
        readonly Dictionary<string, string> files = new(StringComparer.OrdinalIgnoreCase);
        readonly System.Collections.Concurrent.ConcurrentDictionary<string, string> failures = [];
        readonly System.Collections.Concurrent.ConcurrentDictionary<string, Lazy<byte[]?>> images = [];
        static readonly HashSet<string> Shared = ((string)AppContext.GetData("TRUSTED_PLATFORM_ASSEMBLIES")!)
            .Split(Path.PathSeparator).Select(Path.GetFileNameWithoutExtension).ToHashSet(StringComparer.OrdinalIgnoreCase)!;

        public Loader(Workspace workspace)
            : base("dgf-dup-confirm")
        {
            projects = workspace.All.GroupBy(c => c.AssemblyName!).ToDictionary(g => g.Key, g => g.First());
            foreach (var reference in workspace.All.SelectMany(c => c.References).OfType<PortableExecutableReference>())
            {
                if (reference.FilePath is { } path)
                {
                    files.TryAdd(Path.GetFileNameWithoutExtension(path), path);
                }
            }
        }

        protected override Assembly? Load(AssemblyName name)
        {
            if (name.Name is not { } simple || Shared.Contains(simple))
            {
                return null;
            }
            if (projects.TryGetValue(simple, out var compilation))
            {
                return Emit(compilation);
            }
            return files.TryGetValue(simple, out var path) ? LoadFromAssemblyPath(path) : null;
        }

        Assembly? Emit(CSharpCompilation compilation) =>
            Image(compilation) is { } image ? LoadFromStream(new MemoryStream(image)) : null;

        /// <summary>A project compiled, with its source generators run; once, whichever thread asks first.</summary>
        byte[]? Image(CSharpCompilation compilation) => images.GetOrAdd(compilation.AssemblyName!, _ => new Lazy<byte[]?>(() =>
        {
            using var image = new MemoryStream();
            var emitted = Generators.Run(compilation).Emit(image);
            if (!emitted.Success)
            {
                var error = emitted.Diagnostics.First(d => d.Severity == DiagnosticSeverity.Error);
                failures[compilation.AssemblyName!] = $"its project does not compile here ({error.Id}: {error.GetMessage(CultureInfo.InvariantCulture)})";
                return null;
            }
            return image.ToArray();
        })).Value;

        /// <summary>Compiles, in parallel, the projects the methods are in and every project those reference.</summary>
        public void Prepare(IEnumerable<IMethodSymbol> methods)
        {
            var needed = new HashSet<string>();
            var next = new Stack<string>(methods.Select(m => m.ContainingAssembly.Name));
            while (next.TryPop(out var name))
            {
                if (needed.Add(name) && projects.TryGetValue(name, out var compilation))
                {
                    foreach (var referenced in compilation.References.OfType<CompilationReference>())
                    {
                        next.Push(referenced.Compilation.AssemblyName!);
                    }
                }
            }
            Parallel.ForEach(needed.Where(projects.ContainsKey), name => Image(projects[name]));
        }

        /// <summary>Why a type a method needs did not load: its project did not compile, most likely.</summary>
        public string Missing(Exception e)
        {
            var name = e is FileNotFoundException { FileName: { } file } ? new AssemblyName(file).Name : null;
            return name is not null && failures.TryGetValue(name, out var why)
                ? $"{name}: {why}"
                : $"a type it needs did not load ({e.Message})";
        }

        /// <summary>The method a symbol names, loaded; or why it could not be.</summary>
        public (MethodInfo? Method, string? Reason) Method(IMethodSymbol symbol)
        {
            var assemblyName = symbol.ContainingAssembly.Name;
            Assembly? assembly;
            try
            {
                assembly = LoadFromAssemblyName(new AssemblyName(assemblyName));
            }
            catch (Exception e) when (e is FileNotFoundException or FileLoadException or BadImageFormatException)
            {
                return (null, failures.GetValueOrDefault(assemblyName, $"{assemblyName} could not be loaded"));
            }
            var type = assembly.GetType(MetadataName(symbol.ContainingType));
            var method = type?.GetMethods(BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.Static | BindingFlags.Instance | BindingFlags.DeclaredOnly)
                .Where(m => m.Name == symbol.MetadataName && m.GetParameters().Length == symbol.Parameters.Length)
                .FirstOrDefault(m => m.GetParameters().Select(p => p.ParameterType.Name).SequenceEqual(symbol.Parameters.Select(p => ReflectionName(p.Type))));
            return method is null ? (null, $"{symbol.Name} was not found in the compiled code") : (method, null);
        }

        static string MetadataName(INamedTypeSymbol type)
        {
            var names = new List<string>();
            for (var at = type; at is not null; at = at.ContainingType)
            {
                names.Insert(0, at.MetadataName);
            }
            var space = type.ContainingNamespace is { IsGlobalNamespace: false } ns ? ns.ToDisplayString() + "." : "";
            return space + string.Join("+", names);
        }

        static string ReflectionName(ITypeSymbol type) => type switch
        {
            IArrayTypeSymbol array => ReflectionName(array.ElementType) + "[" + new string(',', array.Rank - 1) + "]",
            INamedTypeSymbol { IsValueType: true, OriginalDefinition.SpecialType: SpecialType.System_Nullable_T } => "Nullable`1",
            _ => type.MetadataName,
        };
    }
}

/// <summary>
/// Random inputs from a type and its nullability, with the edge values code
/// tends to branch on; one seed gives the same values every time.
/// </summary>
sealed class Inputs(int seed, Constants constants)
{
    public sealed class CannotMake(string what) : Exception(what);

    readonly Random random = new(seed);

    /// <summary>
    /// Every fourth try is plain: one element at most, and numbers only 0, 1
    /// and the methods' own constants, so a sum lands exactly on a threshold.
    /// </summary>
    readonly bool plain = seed % 4 == 0;
    readonly NullabilityInfoContext nullability = new();
    static readonly string[] Words = ["Ada", "ada", " Ada ", "Bo", "", "  ", "A-1", "b-2", "x@y.z"];
    static readonly decimal[] Amounts = [0m, 1m, 5m, 99.99m, 100m, 100.01m, 250m, -5m];
    static readonly int[] Counts = [0, 1, 2, 10, 100];
    const int Deepest = 3;

    public object?[] Arguments(MethodInfo method) =>
        method.GetParameters().Select(p => Make(p.ParameterType, nullability.Create(p), 0, required: true)).ToArray();

    /// <summary>An object made without its constructor: each field holds a value
    /// generated from its name, so two classes' fields of one name hold the same.</summary>
    public object? Instance(Type type)
    {
        if (type.IsAbstract)
        {
            throw new CannotMake($"{type.Name} is abstract");
        }
        var instance = RuntimeHelpers.GetUninitializedObject(type);
        for (var at = type; at is not null && at != typeof(object); at = at.BaseType)
        {
            foreach (var field in at.GetFields(BindingFlags.Instance | BindingFlags.Public | BindingFlags.NonPublic | BindingFlags.DeclaredOnly))
            {
                var named = new Inputs(HashCode.Combine(seed, Logical(field.Name)), constants);
                field.SetValue(instance, named.Make(field.FieldType, nullability.Create(field), 1, required: false));
            }
        }
        return instance;
    }

    /// <summary><c>_rate</c>, <c>&lt;rate&gt;P</c> (a primary-constructor parameter) and <c>&lt;Rate&gt;k__BackingField</c> are all "rate".</summary>
    static string Logical(string field)
    {
        var name = field.StartsWith('<') ? field[1..field.IndexOf('>')] : field.TrimStart('_');
        return name.ToLowerInvariant();
    }

    object? Make(Type type, NullabilityInfo? info, int depth, bool required)
    {
        if (Null(type, info))
        {
            return null;
        }
        var bare = Nullable.GetUnderlyingType(type) ?? type;
        return Leaf(bare) ?? Composite(bare, info, depth, required);
    }

    /// <summary>Now and then, null where the type allows it.</summary>
    bool Null(Type type, NullabilityInfo? info) =>
        (info?.ReadState == NullabilityState.Nullable || Nullable.GetUnderlyingType(type) is not null) && random.Next(6) == 0;

    /// <summary>A value with no parts: an enum member (before the numbers, as an enum's type code is its integer's), a number, a string, a date.</summary>
    object? Leaf(Type type)
    {
        if (type.IsEnum)
        {
            var values = Enum.GetValues(type);
            return values.GetValue(random.Next(values.Length));
        }
        return Simples.TryGetValue(type, out var make) ? make(this) : null;
    }

    static readonly DateTime Day = new(2026, 1, 1);

    static readonly Dictionary<Type, Func<Inputs, object>> Simples = new()
    {
        [typeof(int)] = i => i.Int(),
        [typeof(long)] = i => (long)i.Int(),
        [typeof(short)] = i => (short)i.Int(),
        [typeof(byte)] = i => (byte)i.random.Next(0, 256),
        [typeof(decimal)] = i => i.Decimal(),
        [typeof(double)] = i => (double)i.Decimal(),
        [typeof(float)] = i => (float)i.Decimal(),
        [typeof(bool)] = i => i.random.Next(2) == 0,
        [typeof(string)] = i => i.Text(),
        [typeof(char)] = i => "aZ0 -"[i.random.Next(5)],
        [typeof(DateTime)] = i => Day.AddDays(i.random.Next(-400, 400)).AddHours(i.random.Next(24)),
        [typeof(Guid)] = i => new Guid(i.random.Next(4), 0, 0, new byte[8]),
        [typeof(TimeSpan)] = i => TimeSpan.FromMinutes(i.random.Next(-60, 600)),
        [typeof(DateTimeOffset)] = i => new DateTimeOffset(Day.AddDays(i.random.Next(-400, 400)), TimeSpan.Zero),
        [typeof(DateOnly)] = i => DateOnly.FromDateTime(Day).AddDays(i.random.Next(-400, 400)),
    };

    /// <summary>A string the methods compare with, now and then; else one of the usual suspects.</summary>
    string Text() => constants.Strings.Count > 0 && random.Next(3) == 0
        ? constants.Strings[random.Next(constants.Strings.Count)]
        : Words[random.Next(Words.Length)];

    /// <summary>A sequence or an object; past <see cref="Deepest"/>, a default.</summary>
    object? Composite(Type type, NullabilityInfo? info, int depth, bool required) =>
        depth >= Deepest ? Default(type) : Sequence(type, info, depth) ?? Construct(type, depth, required);

    static object? Default(Type type) => type.IsValueType ? Activator.CreateInstance(type) : null;

    /// <summary>An array, or a list for a sequence type a list can stand for.</summary>
    object? Sequence(Type type, NullabilityInfo? info, int depth)
    {
        if (type.IsArray)
        {
            return List(type.GetElementType()!, ElementInfo(info, array: true), depth, array: true);
        }
        if (Element(type) is not { } element)
        {
            return null;
        }
        var list = List(element, ElementInfo(info, array: false), depth, array: false);
        return type.IsAssignableFrom(list.GetType()) ? list : null;
    }

    static NullabilityInfo? ElementInfo(NullabilityInfo? info, bool array) =>
        array ? info?.ElementType : info?.GenericTypeArguments.FirstOrDefault();

    int Int() => plain ? (int)Math.Clamp(Plain(), int.MinValue, int.MaxValue) : random.Next(10) switch
    {
        < 3 when constants.Numbers.Count > 0 => (int)Math.Clamp(Near(), int.MinValue, int.MaxValue),
        < 7 => random.Next(-3, 15),
        < 9 => Counts[random.Next(Counts.Length)],
        _ => random.Next(-1000, 1000),
    };

    decimal Decimal() => plain ? Plain() : random.Next(10) switch
    {
        < 3 when constants.Numbers.Count > 0 => Near(),
        < 5 => Amounts[random.Next(Amounts.Length)],
        _ => Math.Round((decimal)(random.NextDouble() * 220 - 20), 2),
    };

    decimal Plain()
    {
        var pick = random.Next(constants.Numbers.Count + 2);
        return pick < constants.Numbers.Count ? constants.Numbers[pick] : pick - constants.Numbers.Count;
    }

    /// <summary>A constant from the methods, or one just either side of it: where <c>&gt;=</c> and <c>&gt;</c> part.</summary>
    decimal Near()
    {
        var value = constants.Numbers[random.Next(constants.Numbers.Count)];
        return random.Next(5) switch
        {
            0 => value - 1,
            1 => value + 1,
            2 => value - 0.01m,
            3 => value + 0.01m,
            _ => value,
        };
    }

    /// <summary>The element type of a sequence type a <c>List&lt;T&gt;</c> can stand for.</summary>
    static Type? Element(Type type) =>
        type.IsGenericType && type.GetGenericArguments().Length == 1 && typeof(IEnumerable).IsAssignableFrom(type)
            ? type.GetGenericArguments()[0]
            : null;

    /// <summary>How many elements: none or one in a plain try, else up to four, empty now and then.</summary>
    int Length() => plain ? random.Next(2) : random.Next(4) == 0 ? 0 : random.Next(1, 5);

    object List(Type element, NullabilityInfo? info, int depth, bool array)
    {
        var length = Length();
        var items = Array.CreateInstance(element, length);
        for (var i = 0; i < length; i++)
        {
            items.SetValue(Make(element, info, depth + 1, required: true), i);
        }
        if (array)
        {
            return items;
        }
        var list = (IList)Activator.CreateInstance(typeof(List<>).MakeGenericType(element))!;
        foreach (var item in items)
        {
            list.Add(item);
        }
        return list;
    }

    /// <summary>A class or struct: by its widest constructor, or without one and its settable properties set.</summary>
    object? Construct(Type type, int depth, bool required)
    {
        if (!Makeable(type))
        {
            return Cannot(type, required);
        }
        try
        {
            var made = Built(type, depth, required);
            Fill(made, type, depth);
            return made;
        }
        catch (TargetInvocationException)
        {
            return Cannot(type, required);
        }
    }

    static bool Makeable(Type type) => !type.IsAbstract && !type.IsInterface && !typeof(Delegate).IsAssignableFrom(type);

    static object? Cannot(Type type, bool required) => required ? throw new CannotMake($"cannot make a {type.Name}") : null;

    object Built(Type type, int depth, bool required)
    {
        var constructor = type.GetConstructors().OrderByDescending(c => c.GetParameters().Length).FirstOrDefault();
        return constructor is null
            ? RuntimeHelpers.GetUninitializedObject(type)
            : constructor.Invoke(constructor.GetParameters().Select(p => Make(p.ParameterType, nullability.Create(p), depth + 1, required)).ToArray());
    }

    void Fill(object made, Type type, int depth)
    {
        foreach (var property in type.GetProperties(BindingFlags.Instance | BindingFlags.Public).Where(Settable))
        {
            property.SetValue(made, Make(property.PropertyType, nullability.Create(property), depth + 1, required: false));
        }
    }

    static bool Settable(PropertyInfo property) => property.SetMethod?.IsPublic == true && property.GetIndexParameters().Length == 0;
}

/// <summary>
/// Values as text that is equal when the values are: decimals without
/// trailing zeros, dictionaries sorted by key, sequences element by element,
/// objects by their public properties.
/// </summary>
static class Show
{
    public static string Value(object? value) => Value(value, 0);

    static string Value(object? value, int depth) =>
        value is null ? "null" : Scalar(value) ?? Structured(value, depth);

    static readonly CultureInfo Invariant = CultureInfo.InvariantCulture;

    static readonly Dictionary<Type, Func<object, string>> Scalars = new()
    {
        [typeof(string)] = v => System.Text.Json.JsonSerializer.Serialize((string)v),
        [typeof(decimal)] = v => ((decimal)v).ToString("G29", Invariant),
        [typeof(double)] = v => ((double)v).ToString("R", Invariant),
        [typeof(float)] = v => ((float)v).ToString("R", Invariant),
        [typeof(bool)] = v => (bool)v ? "true" : "false",
    };

    /// <summary>A value with no parts.</summary>
    static string? Scalar(object value) => Scalars.TryGetValue(value.GetType(), out var show) ? show(value) : value switch
    {
        Enum e => e.GetType().Name + "." + e,
        IFormattable f => f.ToString(null, Invariant),
        _ => null,
    };

    /// <summary>A tuple, a dictionary (sorted by key), a sequence, or an object by its public members.</summary>
    static string Structured(object value, int depth) => value switch
    {
        ITuple tuple => "(" + string.Join(", ", Enumerable.Range(0, tuple.Length).Select(i => Value(tuple[i], depth + 1))) + ")",
        IDictionary map => "{" + string.Join(", ", map.Keys.Cast<object>()
            .Select(k => $"{Value(k, depth + 1)}: {Value(map[k], depth + 1)}")
            .Order(StringComparer.Ordinal)) + "}",
        IEnumerable items => "[" + string.Join(", ", items.Cast<object?>().Take(50).Select(i => Value(i, depth + 1))) + "]",
        _ when depth > 3 => value.GetType().Name,
        _ => Members(value, depth),
    };

    static string Members(object value, int depth)
    {
        var type = value.GetType();
        var properties = type.GetProperties(BindingFlags.Instance | BindingFlags.Public)
            .Where(p => p.GetIndexParameters().Length == 0)
            .Select(p => p.Name + ": " + Read(p, value, depth));
        var fields = type.GetFields(BindingFlags.Instance | BindingFlags.Public)
            .Select(f => f.Name + ": " + Value(f.GetValue(value), depth + 1));
        return type.Name + "(" + string.Join(", ", properties.Concat(fields)) + ")";
    }

    static string Read(PropertyInfo property, object owner, int depth)
    {
        try
        {
            return Value(property.GetValue(owner), depth + 1);
        }
        catch (TargetInvocationException e)
        {
            return "throws " + e.InnerException?.GetType().Name;
        }
    }
}

/// <summary>
/// The numbers and strings the methods compare with: inputs at and around
/// them find where two methods part, which random values rarely hit.
/// </summary>
sealed record Constants(List<decimal> Numbers, List<string> Strings)
{
    public static Constants Of(IEnumerable<Node> expressions)
    {
        var numbers = new SortedSet<decimal>();
        var strings = new SortedSet<string>(StringComparer.Ordinal);
        foreach (var (type, value) in expressions.SelectMany(All).Select(Constant).OfType<(string, string)>())
        {
            if (type == "string")
            {
                strings.Add(System.Text.Json.JsonSerializer.Deserialize<string>(value)!);
            }
            else if (decimal.TryParse(value, NumberStyles.Float, CultureInfo.InvariantCulture, out var number))
            {
                numbers.Add(number);
            }
        }
        return new Constants([.. numbers], [.. strings]);
    }

    /// <summary><c>const:decimal:100</c> as its type and value.</summary>
    static (string, string)? Constant(Node node) =>
        node.StartsWith("const:") && node.Head.Split(':', 3) is [_, var type, var value] ? (type, value) : null;

    static IEnumerable<Node> All(Node n) => n.Kids.SelectMany(All).Prepend(n);
}

/// <summary>
/// The project's own methods by the name a call node carries, reduced on
/// demand: a call into one is run only when its body has no effects either.
/// </summary>
sealed class Callees(Workspace workspace)
{
    readonly Lazy<Dictionary<string, (MethodDeclarationSyntax Syntax, Compilation Compilation)>> methods = new(() =>
        workspace.All.SelectMany(c => c.SyntaxTrees.SelectMany(t => Declared(c, t)))
            .GroupBy(m => m.Name)
            .ToDictionary(g => g.Key, g => (g.First().Syntax, g.First().Compilation)));

    readonly Dictionary<string, Node?> reduced = [];

    readonly Lazy<Dictionary<string, IMethodSymbol>> constructors = new(() =>
        workspace.All.SelectMany(c => c.SyntaxTrees.SelectMany(t =>
            {
                var model = c.GetSemanticModel(t);
                return t.GetRoot().DescendantNodes().OfType<TypeDeclarationSyntax>()
                    .Select(d => model.GetDeclaredSymbol(d)).OfType<INamedTypeSymbol>()
                    .SelectMany(type => type.InstanceConstructors);
            }))
            .GroupBy(ctor => Types.Symbol(ctor.OriginalDefinition))
            .ToDictionary(g => g.Key, g => g.First()));

    /// <summary>
    /// A constructor of the project's own that only stores what it is given:
    /// one the compiler writes (a record's, a primary constructor's, the
    /// default), or one that assigns parameters, constants and <c>?? throw</c>
    /// checks to members; with field and property initializers of constants
    /// and empty collections, and a base constructor of the same kind.
    /// </summary>
    public bool PlainConstructor(string name) =>
        constructors.Value.TryGetValue(name, out var ctor) && Plain(ctor, 0);

    static bool Plain(IMethodSymbol ctor, int depth) =>
        depth <= 4
        && ctor.ContainingType.DeclaringSyntaxReferences.Select(r => r.GetSyntax()).OfType<TypeDeclarationSyntax>().All(PlainMembers)
        && ctor.DeclaringSyntaxReferences.Select(r => r.GetSyntax()).OfType<ConstructorDeclarationSyntax>().All(OnlyStores)
        && PlainBase(ctor.ContainingType.BaseType, depth);

    /// <summary>A constructor's body and the arguments it passes on only store values.</summary>
    static bool OnlyStores(ConstructorDeclarationSyntax syntax)
    {
        var statements = syntax.ExpressionBody is { } arrow
            ? [arrow.Expression]
            : syntax.Body?.Statements.Select(s => (s as ExpressionStatementSyntax)?.Expression) ?? [];
        var chained = syntax.Initializer?.ArgumentList.Arguments.Select(a => a.Expression) ?? [];
        return statements.All(e => e is not null && Stores(e)) && chained.All(Value);
    }

    /// <summary>A base type's constructor runs too: the framework's, or one of the same kind.</summary>
    static bool PlainBase(INamedTypeSymbol? baseType, int depth) =>
        baseType is null || baseType.ContainingAssembly.Name.StartsWith("System", StringComparison.Ordinal)
        || baseType.InstanceConstructors.All(c => c.DeclaringSyntaxReferences.IsEmpty || Plain(c, depth + 1));

    static bool PlainMembers(TypeDeclarationSyntax type) =>
        type.Members.OfType<FieldDeclarationSyntax>().SelectMany(f => f.Declaration.Variables).All(v => v.Initializer is null || Value(v.Initializer.Value))
        && type.Members.OfType<PropertyDeclarationSyntax>().All(p => p.Initializer is null || Value(p.Initializer.Value));

    /// <summary><c>Name = name</c>, <c>_name = name ?? throw new ...</c>, <c>this.x = 0</c>.</summary>
    static bool Stores(ExpressionSyntax expression) =>
        expression is AssignmentExpressionSyntax { Left: IdentifierNameSyntax or MemberAccessExpressionSyntax { Expression: ThisExpressionSyntax } } set
        && Value(set.Right);

    /// <summary>A parameter, a constant, an empty collection or a null check that throws.</summary>
    static bool Value(ExpressionSyntax value) =>
        Plainly.Contains(value.GetType()) || Empty(value) || value switch
        {
            // string.Empty, an enum member
            MemberAccessExpressionSyntax { Expression: PredefinedTypeSyntax or IdentifierNameSyntax } => true,
            BinaryExpressionSyntax { RawKind: (int)Microsoft.CodeAnalysis.CSharp.SyntaxKind.CoalesceExpression, Right: ThrowExpressionSyntax } coalesce => Value(coalesce.Left),
            _ => false,
        };

    static readonly HashSet<Type> Plainly = [typeof(IdentifierNameSyntax), typeof(LiteralExpressionSyntax), typeof(DefaultExpressionSyntax)];

    /// <summary><c>[]</c>, <c>new()</c>, <c>new List&lt;T&gt;()</c>.</summary>
    static bool Empty(ExpressionSyntax value) => value switch
    {
        CollectionExpressionSyntax { Elements.Count: 0 } => true,
        ImplicitObjectCreationExpressionSyntax { ArgumentList.Arguments.Count: 0, Initializer: null } => true,
        ObjectCreationExpressionSyntax { ArgumentList.Arguments.Count: 0, Initializer: null, Type: GenericNameSyntax { Identifier.Text: "List" or "Dictionary" or "HashSet" } } => true,
        _ => false,
    };

    static IEnumerable<(string Name, MethodDeclarationSyntax Syntax, Compilation Compilation)> Declared(Compilation compilation, SyntaxTree tree)
    {
        var model = compilation.GetSemanticModel(tree);
        foreach (var method in tree.GetRoot().DescendantNodes().OfType<MethodDeclarationSyntax>())
        {
            if (model.GetDeclaredSymbol(method) is { } symbol)
            {
                yield return (Types.Symbol(symbol.OriginalDefinition), method, compilation);
            }
        }
    }

    public Node? Expression(string method)
    {
        if (reduced.TryGetValue(method, out var known))
        {
            return known;
        }
        reduced[method] = null; // a recursive call is not followed
        if (!methods.Value.TryGetValue(method, out var found))
        {
            return null;
        }
        var model = found.Compilation.GetSemanticModel(found.Syntax.SyntaxTree);
        // The root only names the file in a report; any folder will do.
        var folder = Path.GetDirectoryName(found.Syntax.SyntaxTree.FilePath) ?? ".";
        return reduced[method] = Reduce.Method(found.Syntax, model, folder, explain: false, small: true)?.Expression;
    }
}
