using System.Globalization;

namespace Shop;

public enum PayloadValueKind { Number, Flag }

public static class PayloadValueParser
{
    public static bool TryParse(PayloadValueKind kind, string? value, out object? parsed)
    {
        parsed = null;
        if (kind == PayloadValueKind.Number && long.TryParse(value, NumberStyles.Integer, CultureInfo.InvariantCulture, out var number))
        {
            parsed = number;
            return true;
        }
        if (kind == PayloadValueKind.Flag && bool.TryParse(value, out var flag))
        {
            parsed = flag;
            return true;
        }
        return false;
    }
}
