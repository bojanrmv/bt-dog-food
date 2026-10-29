// Checks the labels in cases.json against the code: every pair labelled equal
// (and every group) gives the same result on generated inputs, and every pair
// labelled not equal has an input on which it differs. Exit code 1 on a label
// the code does not bear out.

using System.Collections;
using System.Globalization;
using System.Reflection;
using System.Text.Json;
using Corpus.Check;

CultureInfo.CurrentCulture = CultureInfo.InvariantCulture;
var labels = JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "cases.json")));
var failures = 0;
var checkedPairs = 0;

foreach (var group in labels.RootElement.GetProperty("groups").EnumerateArray())
{
    var members = group.GetProperty("members").EnumerateObject().Select(m => m.Name).ToList();
    foreach (var other in members.Skip(1))
    {
        checkedPairs++;
        failures += Report(group.GetProperty("id").GetString()!, members[0], other, equal: true);
    }
}
foreach (var pair in labels.RootElement.GetProperty("pairs").EnumerateArray())
{
    checkedPairs++;
    failures += Report(
        pair.GetProperty("id").GetString()!,
        pair.GetProperty("a").GetString()!,
        pair.GetProperty("b").GetString()!,
        pair.GetProperty("equal").GetBoolean());
}
Console.WriteLine($"{checkedPairs} pairs checked, {failures} labels not borne out");
return failures == 0 ? 0 : 1;

static int Report(string id, string a, string b, bool equal)
{
    var verdict = Compare.Run(Method.Named(a), Method.Named(b), equal ? 500 : 5000);
    var ok = verdict.Differs != equal;
    var what = verdict.Differs ? $"differ: {verdict.Example}" : $"agree on {verdict.Tries} inputs";
    Console.WriteLine($"{(ok ? "ok  " : "FAIL")} {id}: {a} vs {b}: {what}");
    return ok ? 0 : 1;
}

namespace Corpus.Check
{
    static class Method
    {
        /// `Type.Method` in the Corpus namespace.
        public static MethodInfo Named(string name)
        {
            var dot = name.LastIndexOf('.');
            var type = typeof(Corpus.Order).Assembly.GetType("Corpus." + name[..dot])
                ?? throw new ArgumentException($"no type for {name}");
            return type.GetMethod(name[(dot + 1)..], BindingFlags.Public | BindingFlags.Static)
                ?? throw new ArgumentException($"no method {name}");
        }
    }

    record Verdict(bool Differs, int Tries, string Example);

    static class Compare
    {
        /// Runs both methods on the same generated inputs until they differ.
        /// Methods with other parameter types are different by signature.
        public static Verdict Run(MethodInfo a, MethodInfo b, int tries)
        {
            var types = a.GetParameters().Select(p => p.ParameterType).ToArray();
            if (!types.SequenceEqual(b.GetParameters().Select(p => p.ParameterType)))
            {
                return new Verdict(true, 0, "other parameter types");
            }
            var inputs = new Inputs(seed: 7);
            for (var i = 1; i <= tries; i++)
            {
                var args = a.GetParameters().Select(inputs.For).ToArray();
                var left = Result(a, args);
                var right = Result(b, args);
                if (left != right)
                {
                    var shown = string.Join(", ", args.Select(Show.Value));
                    return new Verdict(true, i, $"({shown}) gives {left} vs {right}");
                }
            }
            return new Verdict(false, tries, "");
        }

        static string Result(MethodInfo method, object?[] args)
        {
            try
            {
                return method.ReturnType == typeof(void) ? "void" : Show.Value(method.Invoke(null, args));
            }
            catch (TargetInvocationException e) when (e.InnerException is not null)
            {
                return "throws " + e.InnerException.GetType().Name;
            }
        }
    }

    /// Values as text that is equal when the values are: decimals without
    /// trailing zeros, dictionaries sorted by key, sequences element by element.
    static class Show
    {
        public static string Value(object? value) => value switch
        {
            null => "null",
            string s => JsonSerializer.Serialize(s),
            decimal d => d.ToString("G29", CultureInfo.InvariantCulture),
            double d => d.ToString("R", CultureInfo.InvariantCulture),
            IDictionary map => "{" + string.Join(", ", map.Keys.Cast<object>()
                .Select(k => $"{Value(k)}: {Value(map[k])}")
                .Order(StringComparer.Ordinal)) + "}",
            IEnumerable items => "[" + string.Join(", ", items.Cast<object?>().Select(Value)) + "]",
            Line line => $"Line({Value(line.Sku)}, {line.Quantity}, {Value(line.Price)})",
            Order o => $"Order({o.Id}, {Value(o.Customer)}, {o.Status}, {Value(o.Lines)}, {Value(o.Discount)})",
            IFormattable f => f.ToString(null, CultureInfo.InvariantCulture),
            _ => value.ToString() ?? "",
        };
    }

    /// Random inputs for a parameter, from its type and nullability, with the
    /// edge values code tends to branch on.
    sealed class Inputs(int seed)
    {
        readonly Random random = new(seed);
        readonly NullabilityInfoContext nullability = new();
        static readonly string[] Words = ["Ada", "ada", " Ada ", "Bo", "", "  ", "A-1", "b-2"];
        static readonly decimal[] Amounts = [0m, 1m, 5m, 99.99m, 100m, 100.01m, 250m, -5m];
        static readonly int[] Counts = [0, 1, 2, 10, 100];

        public object? For(ParameterInfo parameter) =>
            Make(parameter.ParameterType, nullability.Create(parameter));

        object? Make(Type type, NullabilityInfo? info)
        {
            if (info?.ReadState == NullabilityState.Nullable && random.Next(6) == 0)
            {
                return null;
            }
            var inner = Nullable.GetUnderlyingType(type);
            if (inner is not null)
            {
                return random.Next(6) == 0 ? null : Make(inner, null);
            }
            if (type == typeof(int)) return Int();
            if (type == typeof(decimal)) return Decimal();
            if (type == typeof(double)) return (double)Decimal();
            if (type == typeof(bool)) return random.Next(2) == 0;
            if (type == typeof(string)) return Words[random.Next(Words.Length)];
            if (type.IsEnum)
            {
                var values = Enum.GetValues(type);
                return values.GetValue(random.Next(values.Length));
            }
            if (type.IsArray)
            {
                return List(type.GetElementType()!, info?.ElementType, array: true);
            }
            if (type.IsGenericType && type.GetGenericArguments().Length == 1
                && typeof(IEnumerable).IsAssignableFrom(type))
            {
                return List(type.GetGenericArguments()[0], info?.GenericTypeArguments.FirstOrDefault(), array: false);
            }
            return Construct(type);
        }

        int Int() => random.Next(10) switch
        {
            < 7 => random.Next(-3, 15),
            < 9 => Counts[random.Next(Counts.Length)],
            _ => random.Next(-1000, 1000),
        };

        decimal Decimal() => random.Next(10) < 3
            ? Amounts[random.Next(Amounts.Length)]
            : Math.Round((decimal)(random.NextDouble() * 220 - 20), 2);

        object List(Type element, NullabilityInfo? info, bool array)
        {
            var length = random.Next(4) == 0 ? 0 : random.Next(1, 5);
            var items = Array.CreateInstance(element, length);
            for (var i = 0; i < length; i++)
            {
                items.SetValue(Make(element, info), i);
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

        object Construct(Type type)
        {
            var constructor = type.GetConstructors().OrderByDescending(c => c.GetParameters().Length).FirstOrDefault()
                ?? throw new ArgumentException($"cannot make a {type.Name}");
            return constructor.Invoke(constructor.GetParameters().Select(For).ToArray());
        }
    }
}
