using System.Text;
using Microsoft.CodeAnalysis;

namespace DgfDup;

/// <summary>
/// Queries that repeat on the same table, and the extension method in the
/// domain that would replace each of them:
/// <list type="bullet">
/// <item>the same filter in several methods: <c>IQueryable&lt;T&gt;.Name(args)</c> with its <c>Where</c>s;</item>
/// <item>the same query (filter, includes, ordering, projection): an extension returning the shaped query;</item>
/// <item>one condition in many queries of a table (a tenant, soft delete,
/// status): a small extension for it, or EF Core's <c>HasQueryFilter</c>
/// when every query needs it.</item>
/// </list>
/// The code is built from the first query's own source, its outside values
/// turned into parameters, so it compiles where that query did.
/// </summary>
public static class QueryGroups
{
    /// <summary>Fewest queries a repeated condition needs before it is worth its own extension.</summary>
    const int ConditionUses = 3;

    public static IEnumerable<Class> Of(IReadOnlyList<QueryUse> uses, string root)
    {
        // A filter of one condition seen twice is too little to name; two conditions, or three queries.
        var filters = Repeated(uses.Where(u => u.Conjuncts.Length > 0 && !ById(u)), u => Name(u.Entity) + "|" + u.Filter.Key)
            .Where(g => g[0].Conjuncts.Length > 1 || g.Count >= ConditionUses).ToList();
        var queries = Repeated(uses.Where(u => !u.Shape.Kids.IsEmpty), u => Name(u.Entity) + "|" + u.Filter.Key + "|" + u.Shape.Key);
        var conditions = Conditions(uses).ToList();
        var shaped = queries.Select(q => Places(q)).ToHashSet();
        // A filter that is one condition repeated is said by that condition's group.
        var named = conditions.Select(c => c.Key).ToHashSet();
        foreach (var group in filters.Where(f => !shaped.Contains(Places(f)) && !named.Contains(Name(f[0].Entity) + "|" + f[0].Filter.Key)))
        {
            yield return FilterClass(group, root);
        }
        foreach (var group in queries)
        {
            yield return QueryClass(group, root);
        }
        foreach (var condition in conditions)
        {
            yield return condition.Class;
        }
    }

    /// <summary>Groups of queries with the same key, in at least two methods.</summary>
    static List<List<QueryUse>> Repeated(IEnumerable<QueryUse> uses, Func<QueryUse, string> key) =>
        uses.GroupBy(key)
            .Select(g => g.OrderBy(u => u.File, StringComparer.Ordinal).ThenBy(u => u.StartLine).ToList())
            .Where(g => g.Select(u => u.File + u.Function).Distinct().Count() > 1)
            .ToList();

    static string Places(List<QueryUse> group) => string.Join(";", group.Select(u => $"{u.File}:{u.StartLine}"));

    /// <summary>Only <c>x.Id == id</c>: what <c>Find</c> is for, not worth an extension.</summary>
    static bool ById(QueryUse use) => use.Conjuncts.Length == 1 && OnKey(use.Conjuncts[0].Atom);

    /// <summary>A condition on the key alone: <c>x.Id == id</c>, <c>ids.Contains(x.Id)</c>.</summary>
    static bool OnKey(Node atom) =>
        atom.Key.Contains(".Id(elem:0)") && atom.Key.Contains("hole:") && Member(atom) == "Id";

    static string Name(ITypeSymbol type) => type.ToDisplayString();

    static string Short(ITypeSymbol type) => type.Name;

    // ---- the classes ----------------------------------------------------------------------

    static Class FilterClass(List<QueryUse> group, string root)
    {
        var first = group[0];
        var method = MethodName(first.Conjuncts);
        var code = Extension(first, method, Wheres(first), $"IQueryable<{Short(first.Entity)}>");
        var unite = $"{group.Count} queries on {Short(first.Entity)} filter it the same way: add this extension to {Short(first.Entity)}Queries {Where(first.Entity, root)} and call .{method}(...) in each";
        return Make(group, code, unite, "the same filter");
    }

    static Class QueryClass(List<QueryUse> group, string root)
    {
        var first = group[0];
        var method = ShapedName(first);
        var last = first.Steps[^1];
        var element = (last.Type as INamedTypeSymbol)?.TypeArguments.FirstOrDefault();
        var result = element is null ? "IQueryable" : $"IQueryable<{Display(element)}>";
        var body = WithHoles(last.Syntax, first, replace: first.Root.Syntax);
        var code = Extension(first, method, body, result);
        var terminals = group.Select(u => u.Terminal).Where(t => t.Length > 0).Distinct().ToList();
        var repository = terminals.Count == 1
            ? $"; they all end with {terminals[0]}, so a repository or query method that runs it is another option"
            : "";
        var unite = $"{group.Count} queries on {Short(first.Entity)} filter and shape it the same way (includes, order, projection): add this extension to {Short(first.Entity)}Queries {Where(first.Entity, root)} and call .{method}(...) in each{repository}";
        return Make(group, code, unite, "the same query");
    }

    /// <summary>One condition repeated across different queries of a table, by its filter key.</summary>
    static IEnumerable<(string Key, Class Class)> Conditions(IReadOnlyList<QueryUse> uses)
    {
        var byCondition = uses
            .SelectMany(u => u.Conjuncts.Where(c => !OnKey(c.Atom)).Select(c => (Use: u, Condition: c)))
            .GroupBy(x => Name(x.Use.Entity) + "|" + Canon.Atom(x.Condition.Atom).Key);
        foreach (var group in byCondition)
        {
            var items = group.GroupBy(x => x.Use).Select(g => g.First())
                .OrderBy(x => x.Use.File, StringComparer.Ordinal).ThenBy(x => x.Use.StartLine).ToList();
            var filters = items.Select(x => x.Use.Filter.Key).Distinct().Count();
            var methods = items.Select(x => x.Use.File + x.Use.Function).Distinct().Count();
            if (items.Count < ConditionUses || filters < 2 || methods < 2)
            {
                continue;
            }
            yield return (group.Key, ConditionClass(items));
        }
    }

    static Class ConditionClass(List<(QueryUse Use, Conjunct Condition)> items)
    {
        var (use, condition) = items[0];
        var method = MethodName([condition]);
        var body = $"query.Where({condition.Parameter} => {WithHoles(condition.Syntax, use)})";
        var code = Extension(use, method, body, $"IQueryable<{Short(use.Entity)}>", condition.Syntax);
        var fixedCondition = !condition.Atom.Key.Contains("hole:");
        var global = fixedCondition
            ? $"; if every query of {Short(use.Entity)} needs it (soft delete, an active flag), HasQueryFilter in the DbContext's model applies it everywhere"
            : $"; if it is the tenant of the request, a global query filter that reads the tenant from the DbContext applies it everywhere";
        var unite = $"{items.Count} different queries on {Short(use.Entity)} repeat this condition: give it a name as an extension in {Short(use.Entity)}Queries and combine it (.{method}(...).Where(...)){global}";
        var group = items.Select(x => x.Use).OrderBy(u => u.File, StringComparer.Ordinal).ThenBy(u => u.StartLine).ToList();
        return Make(group, code, unite, "the same condition");
    }

    static Class Make(List<QueryUse> group, string code, string unite, string what)
    {
        var lines = group.Select(u => u.EndLine + 1 - u.StartLine).ToList();
        var terminals = group.Select(u => u.Terminal).Where(t => t.Length > 0).Distinct().ToList();
        var cautions = terminals.Count > 1
            ? [$"the queries end differently ({string.Join(", ", terminals)}): the extension stops before that, and each keeps its own"]
            : new List<string>();
        return new Class
        {
            Kind = "query",
            CloneType = 0,
            Evidence = "query",
            Entity = Name(group[0].Entity),
            Copies = group.Select(u => new Copy(u.File, u.StartLine, u.EndLine, u.Function, Dedent(u.Code))).ToList(),
            Lines = lines.Max(),
            Template = code,
            Saves = lines.Sum(l => Math.Max(1, l - 1)) - 2,
            Unite = unite,
            Cautions = cautions,
            Rules = group.SelectMany(u => u.Trace.Rules).Append($"{what} on {Short(group[0].Entity)}, by meaning").Distinct().Order(StringComparer.Ordinal).ToList(),
        };
    }

    // ---- code ------------------------------------------------------------------------------

    /// <summary><c>public static IQueryable&lt;T&gt; Name(this IQueryable&lt;T&gt; query, ...) =&gt; body;</c></summary>
    static string Extension(QueryUse use, string method, string body, string result, SyntaxNode? only = null)
    {
        var parameters = Parameters(use, only);
        var list = string.Concat(parameters.Select(p => $", {p.Type} {p.Name}"));
        var entity = Short(use.Entity);
        return new StringBuilder()
            .AppendLine($"public static {result} {method}(this IQueryable<{entity}> query{list}) =>")
            .Append("    ").Append(body).Append(';')
            .ToString();
    }

    /// <summary>The query's filter as <c>query.Where(...)</c>, one per lambda parameter name.</summary>
    static string Wheres(QueryUse use)
    {
        var text = new StringBuilder("query");
        foreach (var run in Runs(use.Conjuncts))
        {
            var parts = string.Join(" && ", run.Select(c => WithHoles(c.Syntax, use)));
            text.Append($".Where({run[0].Parameter} => {parts})");
        }
        return text.ToString();
    }

    /// <summary>Neighbouring conditions with the same parameter name, together.</summary>
    static List<List<Conjunct>> Runs(IEnumerable<Conjunct> conjuncts)
    {
        var runs = new List<List<Conjunct>>();
        foreach (var c in conjuncts)
        {
            if (runs.Count > 0 && runs[^1][0].Parameter == c.Parameter)
            {
                runs[^1].Add(c);
            }
            else
            {
                runs.Add([c]);
            }
        }
        return runs;
    }

    /// <summary>The syntax's text with each outside value as its parameter, and <paramref name="replace"/> as <c>query</c>.</summary>
    static string WithHoles(SyntaxNode syntax, QueryUse use, SyntaxNode? replace = null)
    {
        var names = Parameters(use, null);
        var edits = use.Holes.Select((h, i) => (h.Syntax.Span, Text: names[i].Name))
            .Where(e => syntax.Span.Contains(e.Span))
            .ToList();
        if (replace is not null && syntax.Span.Contains(replace.Span))
        {
            edits.Add((replace.Span, "query"));
        }
        var text = syntax.ToString();
        foreach (var (span, value) in edits.OrderByDescending(e => e.Span.Start))
        {
            var at = span.Start - syntax.Span.Start;
            text = text[..at] + value + text[(at + span.Length)..];
        }
        return Squash(text);
    }

    /// <summary>A parameter per outside value: named after its last identifier, typed as it is.</summary>
    static List<(string Name, string Type)> Parameters(QueryUse use, SyntaxNode? only)
    {
        var all = new List<(string Name, string Type)>();
        foreach (var (syntax, type) in use.Holes)
        {
            var name = Camel(syntax.DescendantTokens().LastOrDefault(t => t.IsKind(Microsoft.CodeAnalysis.CSharp.SyntaxKind.IdentifierToken)).Text);
            var unique = name;
            for (var n = 2; all.Any(p => p.Name == unique); n++)
            {
                unique = name + n;
            }
            all.Add((unique, Display(type)));
        }
        return only is null
            ? all
            : all.Where((p, i) => only.Span.Contains(use.Holes[i].Syntax.Span)).ToList();
    }

    static string Camel(string name) =>
        name.Length == 0 ? "value" : char.ToLowerInvariant(name.TrimStart('_')[0]) + name.TrimStart('_')[1..];

    /// <summary>The code on one line: lines joined by a space, or nothing before a <c>.Call</c>.</summary>
    static string Squash(string text)
    {
        var lines = text.Split((char[])['\r', '\n'], StringSplitOptions.RemoveEmptyEntries).Select(l => l.Trim()).Where(l => l.Length > 0);
        return lines.Aggregate("", (all, line) => all.Length == 0 ? line : all + (line.StartsWith('.') ? "" : " ") + line);
    }

    static string Dedent(string code)
    {
        var lines = code.Replace("\r", "").Split('\n');
        var indent = lines.Skip(1).Where(l => l.Trim().Length > 0).Select(l => l.Length - l.TrimStart().Length).DefaultIfEmpty(0).Min();
        return string.Join("\n", lines.Select((l, i) => i == 0 ? l : l[Math.Min(indent, l.Length - l.TrimStart().Length)..]));
    }

    /// <summary>"next to Order (Domain/Order.cs)": where the extension belongs.</summary>
    static string Where(INamedTypeSymbol entity, string root)
    {
        var file = entity.Locations.FirstOrDefault(l => l.IsInSource)?.SourceTree?.FilePath;
        return file is null
            ? "in the domain"
            : $"next to {entity.Name} ({Path.GetRelativePath(root, file).Replace('\\', '/')})";
    }

    // ---- names -------------------------------------------------------------------------------

    /// <summary>A shaped query's name: its filter's, then what it includes, orders by or selects.</summary>
    static string ShapedName(QueryUse use)
    {
        var filter = use.Conjuncts.Length > 0 ? MethodName(use.Conjuncts) : "";
        var shape = use.Steps.Select(s => s.TargetMethod.Name).ToList();
        var includes = use.Shape.Kids.Where(k => k.Is("step:Include") || k.Is("step:ThenInclude")).Select(k => Member(k) ?? "").Where(n => n.Length > 0).Distinct().Take(2).ToList();
        var part = includes.Count > 0 ? "With" + string.Concat(includes)
            : shape.Contains("Select") ? "Selected"
            : shape.Any(n => n.StartsWith("OrderBy", StringComparison.Ordinal)) ? "Ordered"
            : "Shaped";
        return filter.Length == 0 ? part : filter + part;
    }

    static string Display(ITypeSymbol type) => type.ToDisplayString(SymbolDisplayFormat.MinimallyQualifiedFormat);

    /// <summary>A name from the conditions: <c>x.GymId == id</c> is ForGym, <c>!x.IsDeleted</c> NotDeleted.</summary>
    static string MethodName(IEnumerable<Conjunct> conjuncts)
    {
        var parts = conjuncts.Select(Part).Where(p => p.Length > 0).Distinct().Take(3).ToList();
        return parts.Count == 0 ? "Matching" : string.Concat(parts);
    }

    /// <summary>A condition's part of a name; <c>x.Status == Status.Paid</c> is named by its constant, Paid.</summary>
    static string Part(Conjunct conjunct)
    {
        var part = Part(conjunct.Atom);
        var negated = part.StartsWith("Not", StringComparison.Ordinal);
        if (!NamedByConstant(conjunct.Atom, part) || ConstantName(conjunct.Syntax) is not { } name)
        {
            return part;
        }
        return (negated ? "Not" : "") + name;
    }

    static bool NamedByConstant(Node atom, string part) =>
        (atom.Key.Contains("const:enum") || atom.Key.Contains("const:string"))
        && (part.StartsWith("For", StringComparison.Ordinal) || part.StartsWith("Not", StringComparison.Ordinal));

    /// <summary>The last identifier of a condition's code (an enum member), capitalized.</summary>
    static string? ConstantName(SyntaxNode syntax)
    {
        var last = syntax.DescendantTokens().LastOrDefault(t => t.IsKind(Microsoft.CodeAnalysis.CSharp.SyntaxKind.IdentifierToken)).Text;
        return string.IsNullOrEmpty(last) ? null : char.ToUpperInvariant(last[0]) + last[1..];
    }

    static string Part(Node atom)
    {
        var test = atom.Is("cond") ? atom.Kids[0] : atom;
        var negated = atom.Is("cond") && atom.Kids[1].Equals(Node.False);
        return Member(test) is { } member ? Named(test, member, negated) : "";
    }

    /// <summary>
    /// <c>x.EndedAt == null</c> is WithoutEndedAt, <c>x.GymId == id</c>
    /// ForGym, <c>!x.IsActive</c> NotActive, a comparison or call By....
    /// </summary>
    static string Named(Node test, string member, bool negated)
    {
        var kind = TestKind(test);
        var name = kind == "eq" ? Trim(member, "Id", atStart: false) : Trim(member, "Is", atStart: true);
        return Prefixes[(kind, negated)] + name;
    }

    static readonly Dictionary<(string, bool), string> Prefixes = new()
    {
        [("null", false)] = "Without",
        [("null", true)] = "With",
        [("eq", false)] = "For",
        [("eq", true)] = "Not",
        [("member", false)] = "",
        [("member", true)] = "Not",
        [("other", false)] = "By",
        [("other", true)] = "NotBy",
    };

    /// <summary><c>null</c> for a test against null, <c>eq</c> for another equality, <c>member</c> for a flag, else <c>other</c>.</summary>
    static string TestKind(Node test)
    {
        if (test.Is("eq"))
        {
            return test.Kids.Contains(Node.Null) ? "null" : "eq";
        }
        return test.StartsWith("member:") ? "member" : "other";
    }

    /// <summary><paramref name="name"/> without a leading or trailing <paramref name="part"/>, when more is left.</summary>
    static string Trim(string name, string part, bool atStart)
    {
        if (name.Length <= part.Length)
        {
            return name;
        }
        if (atStart)
        {
            return name.StartsWith(part, StringComparison.Ordinal) ? name[part.Length..] : name;
        }
        return name.EndsWith(part, StringComparison.Ordinal) ? name[..^part.Length] : name;
    }

    /// <summary>The path of members off the element a test reads first: <c>u.Role.Name</c> is RoleName.</summary>
    static string? Member(Node n)
    {
        if (MemberPath(n) is { } path)
        {
            return path;
        }
        return n.Kids.Select(Member).FirstOrDefault(m => m is not null);
    }

    static string? MemberPath(Node n)
    {
        if (!n.StartsWith("member:") || n.Kids.Length != 1)
        {
            return null;
        }
        var name = n.Head["member:".Length..];
        name = name[(name.LastIndexOf('.') + 1)..];
        if (n.Kids[0].StartsWith("elem:"))
        {
            return name;
        }
        return MemberPath(n.Kids[0]) is { } inner ? inner + name : null;
    }
}
