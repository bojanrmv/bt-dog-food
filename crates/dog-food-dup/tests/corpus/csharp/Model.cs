namespace Corpus;

public enum Status
{
    Open,
    Paid,
    Shipped,
    Cancelled,
}

public record Line(string Sku, int Quantity, decimal Price);

public record Order(int Id, string Customer, Status Status, IReadOnlyList<Line> Lines, decimal Discount);
