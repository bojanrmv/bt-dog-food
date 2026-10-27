namespace Shop.Acceptance.Tests;

// Stands in for a slow suite that mutation runs leave out. It sits exactly on
// the discount boundary, so when it runs, `total > 100` -> `>=` is killed;
// when it is excluded, that mutant survives.
public class CheckoutScenarios
{
    [Fact]
    public void A_member_order_of_exactly_100_gets_no_discount() =>
        Assert.Equal(0m, Pricing.Discount(100m, true));
}
