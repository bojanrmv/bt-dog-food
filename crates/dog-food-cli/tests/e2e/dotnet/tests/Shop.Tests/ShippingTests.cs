namespace Shop.Tests;

public class ShippingTests
{
    [Fact]
    public void Weight_must_be_positive() =>
        Assert.Throws<ArgumentOutOfRangeException>(() => Pricing.Shipping(10m, 0));

    [Fact]
    public void Large_orders_ship_free() => Assert.Equal(0m, Pricing.Shipping(80m, 3));

    [Fact]
    public void Heavy_parcels_cost_more() => Assert.Equal(9m, Pricing.Shipping(10m, 20));

    [Fact]
    public void Light_parcels_cost_less() => Assert.Equal(5m, Pricing.Shipping(10m, 2));
}
