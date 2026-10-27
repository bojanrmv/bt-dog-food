using System.Text.RegularExpressions;

namespace Shop;

public static partial class Pricing
{
    /// Members get 10% off orders above 100; the discount is capped at 12.
    public static decimal Discount(decimal total, bool member)
    {
        if (member && total > 100)
        {
            var discount = total * 0.1m;
            return discount > 12 ? 12 : discount;
        }
        return 0;
    }

    /// Shipping is free from 50; heavy parcels cost more.
    public static decimal Shipping(decimal total, int weight)
    {
        if (weight <= 0)
        {
            throw new ArgumentOutOfRangeException(nameof(weight)); // dgf-disable-once statement: a guard clause; callers validate weight first
        }
        if (total >= 50)
        {
            return 0;
        }
        return weight > 10 ? 9 : 5;
    }

    /// A plan is active up to and including its last day.
    public static bool IsActive(DateOnly lastDay, DateOnly today)
    {
        return today <= lastDay;
    }

    /// Order codes look like AB-1234. The regex comes from a source generator,
    /// so the sandbox build must keep generators running.
    public static bool IsOrderCode(string text)
    {
        return text.Length > 0 && OrderCode().IsMatch(text);
    }

    [GeneratedRegex("^[A-Z]{2}-[0-9]{4}$")]
    private static partial Regex OrderCode();
}
