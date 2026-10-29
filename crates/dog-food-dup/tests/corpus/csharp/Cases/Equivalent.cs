using System.Text;

namespace Corpus;

// Groups of methods that do the same thing, written differently (Type 4).
// cases.json names every member; `dotnet run` checks that they agree.

public static class Total
{
    public static decimal For(Order order)
    {
        decimal sum = 0;
        for (var i = 0; i < order.Lines.Count; i++)
        {
            var line = order.Lines[i];
            sum += line.Quantity * line.Price;
        }
        var total = sum - order.Discount;
        return total < 0 ? 0 : total;
    }

    public static decimal Foreach(Order order)
    {
        decimal sum = 0;
        foreach (var line in order.Lines)
        {
            sum = sum + line.Price * line.Quantity;
        }
        if (sum - order.Discount < 0)
        {
            return 0;
        }
        return sum - order.Discount;
    }

    public static decimal While(Order order)
    {
        var sum = 0m;
        var i = 0;
        while (i < order.Lines.Count)
        {
            sum += order.Lines[i].Quantity * order.Lines[i].Price;
            i++;
        }
        return Math.Max(0, sum - order.Discount);
    }

    public static decimal Linq(Order order) =>
        Math.Max(0, order.Lines.Sum(l => l.Quantity * l.Price) - order.Discount);
}

public static class CountPaid
{
    public static int Foreach(IReadOnlyList<Order> orders)
    {
        var count = 0;
        foreach (var order in orders)
        {
            if (order.Status == Status.Paid && order.Discount == 0)
            {
                count++;
            }
        }
        return count;
    }

    public static int For(IReadOnlyList<Order> orders)
    {
        int count = 0;
        for (int i = 0; i < orders.Count; i++)
        {
            if (orders[i].Status != Status.Paid)
            {
                continue;
            }
            if (orders[i].Discount != 0)
            {
                continue;
            }
            count += 1;
        }
        return count;
    }

    public static int Linq(IReadOnlyList<Order> orders) =>
        orders.Count(o => o.Status == Status.Paid && o.Discount == 0);

    public static int WhereCount(IReadOnlyList<Order> orders) =>
        orders.Where(o => o.Status == Status.Paid).Where(o => o.Discount == 0).Count();
}

public static class HasBulkLine
{
    public static bool Foreach(Order order)
    {
        foreach (var line in order.Lines)
        {
            if (line.Quantity >= 10)
            {
                return true;
            }
        }
        return false;
    }

    public static bool Flag(Order order)
    {
        var found = false;
        for (var i = 0; i < order.Lines.Count; i++)
        {
            if (order.Lines[i].Quantity >= 10)
            {
                found = true;
                break;
            }
        }
        return found;
    }

    public static bool Linq(Order order) => order.Lines.Any(l => l.Quantity >= 10);
}

public static class AllDone
{
    public static bool Foreach(IReadOnlyList<Order> orders)
    {
        foreach (var order in orders)
        {
            if (order.Status != Status.Shipped && order.Status != Status.Cancelled)
            {
                return false;
            }
        }
        return true;
    }

    public static bool Linq(IReadOnlyList<Order> orders) =>
        orders.All(o => o.Status == Status.Shipped || o.Status == Status.Cancelled);

    public static bool NotAny(IReadOnlyList<Order> orders) =>
        !orders.Any(o => !(o.Status == Status.Shipped || o.Status == Status.Cancelled));
}

public static class FirstBigOrder
{
    public static int? Foreach(IReadOnlyList<Order> orders)
    {
        foreach (var order in orders)
        {
            if (order.Lines.Count > 2)
            {
                return order.Id;
            }
        }
        return null;
    }

    public static int? For(IReadOnlyList<Order> orders)
    {
        int? result = null;
        for (var i = 0; i < orders.Count; i++)
        {
            if (orders[i].Lines.Count > 2)
            {
                result = orders[i].Id;
                break;
            }
        }
        return result;
    }

    public static int? Linq(IReadOnlyList<Order> orders) =>
        orders.FirstOrDefault(o => o.Lines.Count > 2)?.Id;
}

public static class Restock
{
    public static List<string> Foreach(Order order)
    {
        var skus = new List<string>();
        foreach (var line in order.Lines)
        {
            if (line.Quantity < 0)
            {
                skus.Add(line.Sku.ToUpperInvariant());
            }
        }
        return skus;
    }

    public static List<string> For(Order order)
    {
        var result = new List<string>();
        for (var i = 0; i < order.Lines.Count; i++)
        {
            var line = order.Lines[i];
            if (!(line.Quantity >= 0))
            {
                result.Add(line.Sku.ToUpperInvariant());
            }
        }
        return result;
    }

    public static List<string> Linq(Order order) =>
        order.Lines.Where(l => l.Quantity < 0).Select(l => l.Sku.ToUpperInvariant()).ToList();
}

public static class ShippingFee
{
    public static decimal Guards(Status status, decimal subtotal)
    {
        if (status == Status.Cancelled)
        {
            return 0;
        }
        if (subtotal >= 100)
        {
            return 0;
        }
        if (status == Status.Paid)
        {
            return 5;
        }
        return 8;
    }

    public static decimal Nested(Status status, decimal subtotal)
    {
        decimal fee;
        if (status != Status.Cancelled)
        {
            if (subtotal < 100)
            {
                if (status == Status.Paid)
                {
                    fee = 5;
                }
                else
                {
                    fee = 8;
                }
            }
            else
            {
                fee = 0;
            }
        }
        else
        {
            fee = 0;
        }
        return fee;
    }

    public static decimal Conditional(Status status, decimal subtotal) =>
        status == Status.Cancelled || subtotal >= 100 ? 0 : status == Status.Paid ? 5 : 8;

    public static decimal Switch(Status status, decimal subtotal) =>
        status switch
        {
            Status.Cancelled => 0,
            _ when subtotal >= 100 => 0,
            Status.Paid => 5,
            _ => 8,
        };
}

public static class Validate
{
    public static string EarlyReturn(Order order)
    {
        if (order.Lines.Count == 0)
        {
            return "empty";
        }
        if (order.Discount < 0)
        {
            return "bad discount";
        }
        if (order.Status == Status.Cancelled)
        {
            return "cancelled";
        }
        return "ok";
    }

    public static string Nested(Order order)
    {
        string result;
        if (order.Lines.Count > 0)
        {
            if (order.Discount >= 0)
            {
                if (order.Status != Status.Cancelled)
                {
                    result = "ok";
                }
                else
                {
                    result = "cancelled";
                }
            }
            else
            {
                result = "bad discount";
            }
        }
        else
        {
            result = "empty";
        }
        return result;
    }
}

public static class StockLabel
{
    public static string Plain(Line line)
    {
        if (line.Quantity > 0)
        {
            var amount = line.Quantity * line.Price;
            return line.Sku + ": " + amount;
        }
        else
        {
            return line.Sku + ": none";
        }
    }

    public static string Inverted(Line line)
    {
        if (line.Quantity <= 0)
        {
            return line.Sku + ": none";
        }
        else
        {
            var amount = line.Quantity * line.Price;
            return line.Sku + ": " + amount;
        }
    }
}

public static class Gross
{
    public static decimal Locals(Line line)
    {
        var net = line.Quantity * line.Price;
        var tax = net * 0.2m;
        var gross = net + tax;
        return Math.Round(gross, 2);
    }

    public static decimal Inline(Line line)
    {
        return Math.Round(line.Quantity * line.Price + line.Quantity * line.Price * 0.2m, 2);
    }
}

public static class Summary
{
    public static string CountFirst(Order order)
    {
        var count = order.Lines.Count;
        var units = 0;
        foreach (var line in order.Lines)
        {
            units += line.Quantity;
        }
        var name = order.Customer.Trim();
        return $"{name}: {count} lines, {units} units";
    }

    public static string NameFirst(Order order)
    {
        var name = order.Customer.Trim();
        var units = 0;
        foreach (var line in order.Lines)
        {
            units += line.Quantity;
        }
        var count = order.Lines.Count;
        return $"{name}: {count} lines, {units} units";
    }
}

public static class Describe
{
    public static string Concat(Line line)
    {
        var text = line.Sku + " x" + line.Quantity + " @ " + line.Price;
        if (line.Quantity > 1)
        {
            text = text + " each";
        }
        return text;
    }

    public static string Interpolated(Line line)
    {
        var text = $"{line.Sku} x{line.Quantity} @ {line.Price}";
        if (line.Quantity > 1)
        {
            text += " each";
        }
        return text;
    }

    public static string Builder(Line line)
    {
        var text = new StringBuilder();
        text.Append(line.Sku).Append(" x").Append(line.Quantity).Append(" @ ").Append(line.Price);
        if (line.Quantity > 1)
        {
            text.Append(" each");
        }
        return text.ToString();
    }
}

public static class GuestName
{
    public static string EqualsNull(string? name)
    {
        if (name == null || name.Trim().Length == 0)
        {
            return "guest";
        }
        return name.Trim();
    }

    public static string IsNull(string? name)
    {
        if (name is null)
        {
            return "guest";
        }
        var trimmed = name.Trim();
        return trimmed.Length == 0 ? "guest" : trimmed;
    }

    public static string Conditional(string? name)
    {
        var trimmed = name?.Trim();
        return string.IsNullOrEmpty(trimmed) ? "guest" : trimmed;
    }
}

public static class CanShip
{
    public static bool Negated(Order order)
    {
        if (!(order.Status == Status.Paid && order.Lines.Count > 0))
        {
            return false;
        }
        return order.Lines.All(l => l.Quantity > 0);
    }

    public static bool DeMorgan(Order order)
    {
        if (order.Status != Status.Paid || order.Lines.Count == 0)
        {
            return false;
        }
        foreach (var line in order.Lines)
        {
            if (line.Quantity <= 0)
            {
                return false;
            }
        }
        return true;
    }
}

public static class Largest
{
    public static int Loop(IReadOnlyList<Line> lines)
    {
        if (lines.Count == 0)
        {
            return 0;
        }
        var max = lines[0].Quantity;
        foreach (var line in lines)
        {
            if (line.Quantity > max)
            {
                max = line.Quantity;
            }
        }
        return max;
    }

    public static int Max(IReadOnlyList<Line> lines) =>
        lines.Count == 0 ? 0 : lines.Max(l => l.Quantity);

    public static int DefaultIfEmpty(IReadOnlyList<Line> lines) =>
        lines.Select(l => l.Quantity).DefaultIfEmpty(0).Max();
}

public static class TotalsByCustomer
{
    public static Dictionary<string, decimal> TryGet(IReadOnlyList<Order> orders)
    {
        var totals = new Dictionary<string, decimal>();
        foreach (var order in orders)
        {
            decimal sum = 0;
            foreach (var line in order.Lines)
            {
                sum += line.Quantity * line.Price;
            }
            if (totals.TryGetValue(order.Customer, out var current))
            {
                totals[order.Customer] = current + sum;
            }
            else
            {
                totals[order.Customer] = sum;
            }
        }
        return totals;
    }

    public static Dictionary<string, decimal> GetValueOrDefault(IReadOnlyList<Order> orders)
    {
        var totals = new Dictionary<string, decimal>();
        foreach (var order in orders)
        {
            var sum = order.Lines.Sum(l => l.Quantity * l.Price);
            totals[order.Customer] = totals.GetValueOrDefault(order.Customer) + sum;
        }
        return totals;
    }

    public static Dictionary<string, decimal> GroupBy(IReadOnlyList<Order> orders) =>
        orders
            .GroupBy(o => o.Customer)
            .ToDictionary(g => g.Key, g => g.Sum(o => o.Lines.Sum(l => l.Quantity * l.Price)));
}
