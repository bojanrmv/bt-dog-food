namespace Shop;

/// Built once, inside whichever test first touches the class.
public static class Codes
{
    public static readonly IReadOnlyList<int> All = Build();

    private static List<int> Build()
    {
        var list = new List<int>();
        for (var i = 0; i < 3; i++)
        {
            list.Add(i * 10);
        }
        return list;
    }
}
