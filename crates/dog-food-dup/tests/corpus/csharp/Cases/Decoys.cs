namespace Corpus;

// Same shape, unrelated purpose: uniting these would couple code that only
// looks alike. A tool that reports them is wrong (or should rank them last).

public static class Csv
{
    public static string Row(Line line)
    {
        var parts = new List<string>();
        parts.Add(line.Sku);
        parts.Add(line.Quantity.ToString());
        parts.Add(line.Price.ToString());
        parts.Add((line.Quantity * line.Price).ToString());
        return string.Join(";", parts);
    }

    public static void CheckLine(Line line)
    {
        if (string.IsNullOrWhiteSpace(line.Sku))
        {
            throw new ArgumentException("A line needs a SKU.");
        }
        if (line.Price < 0)
        {
            throw new ArgumentException("A price cannot be negative.");
        }
        if (line.Quantity > 1000)
        {
            throw new ArgumentException("Too many units on one line.");
        }
    }
}

public static class AuditLog
{
    public static string Entry(Order order)
    {
        var parts = new List<string>();
        parts.Add(order.Customer);
        parts.Add(order.Id.ToString());
        parts.Add(order.Status.ToString());
        parts.Add(order.Lines.Count.ToString());
        return string.Join(" | ", parts);
    }

    public static void CheckOrder(Order order)
    {
        if (string.IsNullOrWhiteSpace(order.Customer))
        {
            throw new ArgumentException("An order needs a customer.");
        }
        if (order.Discount < 0)
        {
            throw new ArgumentException("A discount cannot be negative.");
        }
        if (order.Lines.Count > 50)
        {
            throw new ArgumentException("Too many lines on one order.");
        }
    }
}
