// Queries on one table, for the repeated-query finder (tests/corpus.rs).
namespace Shop;

public class Order
{
    public int Id { get; set; }
    public Guid ShopId { get; set; }
    public bool IsDeleted { get; set; }
    public decimal Total { get; set; }
    public DateTime PlacedAt { get; set; }
}

public record Request(Guid ShopId);

public class Context
{
    readonly List<Order> orders = [];
    public IQueryable<Order> Orders => orders.AsQueryable();
}

public class OpenOrders(Context db)
{
    public List<Order> List(Guid shopId) =>
        db.Orders.Where(o => o.ShopId == shopId && !o.IsDeleted).ToList();
}

public class OrderCounts(Context db)
{
    // The same filter: split in two, in the other order, renamed.
    public int Count(Guid shop) =>
        db.Orders.Where(x => !x.IsDeleted).Where(x => x.ShopId == shop).Count();
}

public class OrderLookup(Context db)
{
    // The same filter, as FirstOrDefault's predicate, from a request.
    public Order? First(Request request) =>
        db.Orders.FirstOrDefault(o => o.ShopId == request.ShopId && !o.IsDeleted);
}

public class DeletedOrders(Context db)
{
    // Not the same: the deleted ones.
    public List<Order> List(Guid shopId) =>
        db.Orders.Where(o => o.ShopId == shopId && o.IsDeleted).ToList();
}

public class BigOrders(Context db)
{
    // Another filter that repeats the soft-delete condition.
    public List<Order> List() =>
        db.Orders.Where(o => !o.IsDeleted && o.Total > 100).ToList();
}

public class RecentTotals(Context db)
{
    public List<decimal> Totals(Guid shopId) =>
        db.Orders.Where(o => o.ShopId == shopId).OrderByDescending(o => o.PlacedAt).Select(o => o.Total).ToList();
}

public class TotalsReport(Context db)
{
    public decimal[] Latest(Guid id) =>
        db.Orders.Where(order => order.ShopId == id).OrderByDescending(order => order.PlacedAt).Select(order => order.Total).ToArray();
}
