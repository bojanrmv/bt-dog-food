namespace Corpus;

// Pairs that look alike but do not do the same thing. A tool may report them
// as similar code (most are one edit apart), but must never call them equal;
// `dotnet run` finds an input on which each pair differs.

public static class FreeShipping
{
    public static bool AtLeast(Order order)
    {
        var subtotal = 0m;
        foreach (var line in order.Lines)
        {
            subtotal += line.Quantity * line.Price;
        }
        return order.Status != Status.Cancelled && subtotal >= 100;
    }

    public static bool Above(Order order)
    {
        var subtotal = 0m;
        foreach (var line in order.Lines)
        {
            subtotal += line.Quantity * line.Price;
        }
        return order.Status != Status.Cancelled && subtotal > 100;
    }
}

public static class QuantityOf
{
    public static int First(IReadOnlyList<Line> lines, string sku)
    {
        var line = lines.First(l => l.Sku == sku);
        var quantity = line.Quantity;
        if (quantity < 0)
        {
            quantity = 0;
        }
        return quantity;
    }

    public static int FirstOrDefault(IReadOnlyList<Line> lines, string sku)
    {
        var line = lines.FirstOrDefault(l => l.Sku == sku);
        var quantity = line?.Quantity ?? 0;
        if (quantity < 0)
        {
            quantity = 0;
        }
        return quantity;
    }
}

public static class InStock
{
    public static bool Any(Order order)
    {
        if (order.Status == Status.Cancelled)
        {
            return false;
        }
        var ready = order.Lines.Any(l => l.Quantity > 0);
        return ready && order.Lines.Count > 0;
    }

    public static bool All(Order order)
    {
        if (order.Status == Status.Cancelled)
        {
            return false;
        }
        var ready = order.Lines.All(l => l.Quantity > 0);
        return ready && order.Lines.Count > 0;
    }
}

public static class Greeting
{
    public static string Checked(string? name, Status status)
    {
        if (name == null)
        {
            return "Hello";
        }
        var text = "Hello " + name.Trim();
        if (status == Status.Paid)
        {
            text += ", thanks";
        }
        return text;
    }

    public static string Unchecked(string? name, Status status)
    {
        var text = "Hello " + name!.Trim();
        if (status == Status.Paid)
        {
            text += ", thanks";
        }
        return text;
    }
}

public static class PaidShare
{
    static decimal Share(decimal part, decimal whole) =>
        whole == 0 ? 0 : Math.Round(part / whole, 4);

    public static decimal InOrder(Order order, decimal paid)
    {
        var total = 0m;
        foreach (var line in order.Lines)
        {
            total += line.Quantity * line.Price;
        }
        return Share(paid, total);
    }

    public static decimal Swapped(Order order, decimal paid)
    {
        var total = 0m;
        foreach (var line in order.Lines)
        {
            total += line.Quantity * line.Price;
        }
        return Share(total, paid);
    }
}

public static class AverageUnits
{
    public static decimal IntegerDivision(Order order)
    {
        if (order.Lines.Count == 0)
        {
            return 0;
        }
        var units = 0;
        foreach (var line in order.Lines)
        {
            units += line.Quantity;
        }
        return units / order.Lines.Count;
    }

    public static decimal DecimalDivision(Order order)
    {
        if (order.Lines.Count == 0)
        {
            return 0;
        }
        var units = 0;
        foreach (var line in order.Lines)
        {
            units += line.Quantity;
        }
        return (decimal)units / order.Lines.Count;
    }
}

public static class UnitsBefore
{
    public static int AllLines(Order order)
    {
        var units = 0;
        for (var i = 0; i < order.Lines.Count; i++)
        {
            units += order.Lines[i].Quantity;
        }
        return units;
    }

    public static int SkipsLast(Order order)
    {
        var units = 0;
        for (var i = 0; i < order.Lines.Count - 1; i++)
        {
            units += order.Lines[i].Quantity;
        }
        return units;
    }
}

public static class SameCustomer
{
    public static bool Ordinal(Order a, Order b)
    {
        if (a.Id == b.Id)
        {
            return true;
        }
        var left = a.Customer.Trim();
        var right = b.Customer.Trim();
        return string.Equals(left, right, StringComparison.Ordinal);
    }

    public static bool IgnoreCase(Order a, Order b)
    {
        if (a.Id == b.Id)
        {
            return true;
        }
        var left = a.Customer.Trim();
        var right = b.Customer.Trim();
        return string.Equals(left, right, StringComparison.OrdinalIgnoreCase);
    }
}

public static class Waiting
{
    public static int Paid(IReadOnlyList<Order> orders)
    {
        var count = 0;
        foreach (var order in orders)
        {
            if (order.Status == Status.Paid && order.Lines.Count > 0)
            {
                count++;
            }
        }
        return count;
    }

    public static int Shipped(IReadOnlyList<Order> orders)
    {
        var count = 0;
        foreach (var order in orders)
        {
            if (order.Status == Status.Shipped && order.Lines.Count > 0)
            {
                count++;
            }
        }
        return count;
    }
}

public static class LineSum
{
    public static decimal Amounts(Order order)
    {
        var sum = 0m;
        foreach (var line in order.Lines)
        {
            sum += line.Quantity * line.Price;
        }
        return sum - order.Discount;
    }

    public static decimal Prices(Order order)
    {
        var sum = 0m;
        foreach (var line in order.Lines)
        {
            sum += line.Price;
        }
        return sum - order.Discount;
    }
}
