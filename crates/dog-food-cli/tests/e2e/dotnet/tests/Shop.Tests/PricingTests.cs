namespace Shop.Tests;

// Deliberately incomplete: no test sits on a boundary (a total of exactly 100,
// a total of exactly 50, a plan on its last day), so those mutants survive.
public class PricingTests
{
    [Fact]
    public void Members_get_the_capped_discount() => Assert.Equal(12m, Pricing.Discount(200m, true));

    [Fact]
    public void Members_get_ten_percent_below_the_cap() => Assert.Equal(11m, Pricing.Discount(110m, true));

    [Fact]
    public void Small_orders_get_nothing() => Assert.Equal(0m, Pricing.Discount(50m, true));

    [Fact]
    public void Guests_get_nothing() => Assert.Equal(0m, Pricing.Discount(200m, false));

    [Fact]
    public void Plans_end_after_their_last_day() =>
        Assert.False(Pricing.IsActive(new DateOnly(2026, 1, 31), new DateOnly(2026, 2, 1)));

    [Fact]
    public void Plans_run_before_their_last_day() =>
        Assert.True(Pricing.IsActive(new DateOnly(2026, 1, 31), new DateOnly(2026, 1, 15)));

    [Theory]
    [InlineData("AB-1234", true)]
    [InlineData("AB-12", false)]
    [InlineData("", false)]
    public void Order_codes(string text, bool valid) => Assert.Equal(valid, Pricing.IsOrderCode(text));
}
