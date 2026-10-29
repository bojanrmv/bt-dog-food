namespace DgfDup;

/// <summary>
/// Which values can be copied to every place a local is used without
/// changing what runs: constants, parameters, reads of members, operators,
/// LINQ over them, and the framework's string and math functions. Calls to
/// anything else, awaits and new objects have effects (or an identity) and
/// are bound once instead.
/// </summary>
public static class Purity
{
    public static bool IsPure(Node n) => PureHead(n) && n.Kids.All(IsPure);

    static bool PureHead(Node n)
    {
        if (n.StartsWith("call:"))
        {
            return PureCalls.Any(prefix => n.Head.StartsWith(prefix, StringComparison.Ordinal));
        }
        return !n.StartsWith("await") && !n.StartsWith("new:") && !n.StartsWith("anonymous") && !n.StartsWith("collection:");
    }

    static readonly string[] PureCalls =
    [
        "call:string.", "call:System.Math.", "call:System.MathF.", "call:int.", "call:decimal.", "call:double.",
        "call:long.", "call:bool.", "call:char.", "call:System.DateTime.", "call:System.DateTimeOffset.",
        "call:System.TimeSpan.", "call:System.Guid.ToString", "call:object.ToString", "call:System.Enum.",
    ];
}
