namespace Corpus;

// Copies (Types 1-3): code pasted and then edited, the classic duplicates.

public static class Invoices
{
    // Type 1 with Receipts.Header: the same code.
    public static string Header(Order order)
    {
        var lines = new List<string>();
        lines.Add("Order " + order.Id);
        lines.Add("Customer: " + order.Customer.Trim());
        if (order.Status == Status.Cancelled)
        {
            lines.Add("CANCELLED");
        }
        lines.Add(new string('-', 20));
        return string.Join("\n", lines);
    }

    // Type 2 with Receipts.Footer: renamed, other texts.
    public static string Footer(Order order)
    {
        decimal subtotal = 0;
        foreach (var line in order.Lines)
        {
            subtotal += line.Quantity * line.Price;
        }
        var text = "Invoice total: " + (subtotal - order.Discount);
        if (order.Discount > 0)
        {
            text += " (discount " + order.Discount + ")";
        }
        return text;
    }

    // Type 3 with Receipts.Lines: Receipts adds a statement.
    public static List<string> Lines(Order order)
    {
        var rows = new List<string>();
        foreach (var line in order.Lines)
        {
            var amount = line.Quantity * line.Price;
            rows.Add(line.Sku + " " + line.Quantity + " " + amount);
        }
        rows.Sort(StringComparer.Ordinal);
        return rows;
    }

    // Type 3 with Receipts.Due: one statement changed.
    public static decimal Due(Order order, decimal paid)
    {
        var total = order.Lines.Sum(l => l.Quantity * l.Price) - order.Discount;
        if (total < 0)
        {
            total = 0;
        }
        var due = total - paid;
        if (due < 0)
        {
            due = 0;
        }
        return Math.Round(due, 2);
    }
}

public static class Receipts
{
    public static string Header(Order order)
    {
        var lines = new List<string>();
        lines.Add("Order " + order.Id);
        lines.Add("Customer: " + order.Customer.Trim());
        if (order.Status == Status.Cancelled)
        {
            lines.Add("CANCELLED");
        }
        lines.Add(new string('-', 20));
        return string.Join("\n", lines);
    }

    public static string Footer(Order receipt)
    {
        decimal sum = 0;
        foreach (var item in receipt.Lines)
        {
            sum += item.Quantity * item.Price;
        }
        var label = "Paid: " + (sum - receipt.Discount);
        if (receipt.Discount > 0)
        {
            label += " (saved " + receipt.Discount + ")";
        }
        return label;
    }

    public static List<string> Lines(Order order)
    {
        var rows = new List<string>();
        foreach (var line in order.Lines)
        {
            if (line.Quantity == 0)
            {
                continue;
            }
            var amount = line.Quantity * line.Price;
            rows.Add(line.Sku + " " + line.Quantity + " " + amount);
        }
        rows.Sort(StringComparer.Ordinal);
        return rows;
    }

    public static decimal Due(Order order, decimal paid)
    {
        var total = order.Lines.Sum(l => l.Quantity * l.Price) - order.Discount;
        if (total < 0)
        {
            total = 0;
        }
        var due = total - paid * 1.02m;
        if (due < 0)
        {
            due = 0;
        }
        return Math.Round(due, 2);
    }
}
