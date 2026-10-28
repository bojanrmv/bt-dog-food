namespace Shop.Tests;

public class CatalogTouchTests
{
    [Fact]
    public void The_catalog_exists() => Assert.NotNull(Catalog.Get());
}

public class CatalogSizeTests
{
    [Fact]
    public void The_catalog_holds_six() => Assert.Equal(6, Catalog.Get().Size);
}
