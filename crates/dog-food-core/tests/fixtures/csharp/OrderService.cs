// Fixture: a `dgf: N` comment states the expected complexity of the function
// that starts on the next line.

using System;
using System.Collections.Generic;

namespace Shop.Orders;

public class OrderService
{
    private readonly Dictionary<string, decimal> _prices = new();

    // dgf: 1
    public OrderService(Dictionary<string, decimal> prices)
    {
        _prices = prices;
    }

    // dgf: 4
    public int Classify(int x)
    {
        if (x < 0) return -1;
        if (x == 0) return 0;
        return x > 100 ? 2 : 1;
    }

    // dgf: 8 cognitive=6 nesting=2 params=2
    public decimal CalculateDiscount(Order order, Customer? customer)
    {
        if (order.Lines.Count == 0)
        {
            return 0m;
        }

        var discount = 0m;
        foreach (var line in order.Lines)
        {
            if (line.Quantity >= 10 && line.Price > 5m)
            {
                discount += line.Price * 0.05m;
            }
        }

        var tier = customer?.Tier ?? "none";
        switch (tier)
        {
            case "gold":
                discount += 10m;
                break;
            case "silver":
                discount += 5m;
                break;
            default:
                break;
        }

        return discount;
    }

    // dgf: 4
    public string Describe(Order order) => order.Status switch
    {
        Status.New => "New",
        Status.Paid => "Paid",
        Status.Shipped => "Shipped",
        _ => "Unknown",
    };

    // dgf: 3
    public bool TryLoad(string id, out Order? order)
    {
        try
        {
            order = Repository.Load(id);
            return order is not null;
        }
        catch (TimeoutException)
        {
            order = null;
            return false;
        }
        catch (Exception)
        {
            throw;
        }
    }

    public int Pending
    {
        // dgf: 2
        get
        {
            return _prices.Count > 0 ? _prices.Count : 0;
        }
    }
}
