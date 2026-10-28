namespace Shop;

/// Built once per process and cached, like an EF Core model: its setup runs
/// inside whichever test asks first.
public sealed class Catalog
{
    private static Catalog? built;

    public int Size { get; private set; }

    public static Catalog Get() => built ??= Build();

    private static Catalog Build()
    {
        var catalog = new Catalog();
        OnModelCreating(catalog);
        return catalog;
    }

    private static void OnModelCreating(Catalog catalog)
    {
        catalog.Size = 2 * 3;
    }
}
