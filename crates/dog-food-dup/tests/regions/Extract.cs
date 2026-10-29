// Fixture for the C# signatures: a copy inside two bigger methods that reads
// a local set before it and sets one read after it.
namespace Shop.Regions;

public sealed record Line(string Sku, int Quantity, decimal Price);

public sealed class Invoices
{
    readonly decimal fee = 2.5m;

    public string Total(IReadOnlyList<Line> lines, string customer)
    {
        var heading = "Invoice for " + customer.Trim();
        var rate = customer.StartsWith("VIP") ? 0.9m : 1m;
        var subtotal = 0m;
        foreach (var line in lines)
        {
            subtotal += line.Quantity * line.Price;
        }
        var discounted = subtotal * rate;
        var total = discounted + fee + (discounted > 100m ? 0m : 4.99m);
        var rounded = Math.Round(total, 2);
        return heading + ": " + rounded;
    }

    public string Receipt(IReadOnlyList<Line> items, string buyer)
    {
        var title = "Receipt for " + buyer.ToUpperInvariant();
        var rate = buyer.EndsWith("!") ? 0.8m : 1m;
        var subtotal = 0m;
        foreach (var line in items)
        {
            subtotal += line.Quantity * line.Price;
        }
        var discounted = subtotal * rate;
        var total = discounted + fee + (discounted > 100m ? 0m : 4.99m);
        var rounded = Math.Round(total, 2);
        return title + " = " + rounded + " paid";
    }
}
