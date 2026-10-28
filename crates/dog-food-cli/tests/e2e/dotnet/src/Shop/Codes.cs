using System.Collections.Concurrent;

namespace Shop;

/// Built once, inside whichever test first touches the class.
public static class Codes
{
    public static readonly IReadOnlyList<int> All = Build();

    private static readonly ConcurrentDictionary<int, int> Squares = new();

    private static List<int> Build()
    {
        var list = new List<int>();
        foreach (var step in new[] { 0, 1, 2 })
        {
            list.Add(step * 10);
        }
        return list;
    }

    /// Filled on first use, inside whichever test asks first.
    public static int Square(int n) => Squares.GetOrAdd(n, k => k * k);
}
