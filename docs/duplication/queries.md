# Repeated queries on a table

[Duplication](README.md) › Repeated queries

With `--semantic`, every LINQ query on `IQueryable<T>` (an EF Core `DbSet<T>`, or a query built on one) is read on its own, knowing which entity `T`, which table, it queries. Its filter is reduced to one form:

- lambda parameter names do not matter;
- `Where(a).Where(b)` is `Where(a && b)`, in any order, as SQL `AND` does not care;
- `FirstOrDefaultAsync(pred)` is `Where(pred).FirstOrDefaultAsync()`;
- values from outside the lambdas (`request.ShopId`, `shopId`) become parameters.

## What is reported

Each comes with the extension method that would replace it, built from the first query's own code, for an `{Entity}Queries` class next to the entity.

| Kind | Example | Suggestion |
| --- | --- | --- |
| The same filter in several methods | `o.ShopId == shopId && !o.IsDeleted` | `ForShopNotDeleted(this IQueryable<Order> query, Guid shopId)` |
| The same whole query: filter, includes, order, projection | `.Where(...).OrderByDescending(o => o.PlacedAt).Select(o => o.Total)` | an extension returning the shaped `IQueryable<decimal>` |
| One condition in three or more different queries | `!u.IsDeleted` | a small named extension (`NotDeleted()`), or EF Core's `HasQueryFilter` when every query needs it |

```csharp
public static class OrderQueries
{
    public static IQueryable<Order> ForShopNotDeleted(this IQueryable<Order> query, Guid shopId) =>
        query.Where(o => o.ShopId == shopId && !o.IsDeleted);
}
```

Names come from the conditions: `ForShop` (`ShopId == ...`), `NotDeleted` (`!IsDeleted`), `WithoutEndedAt` (`EndedAt == null`), `Confirmed` (`Status == Status.Confirmed`). They are suggestions, to rename.

Conditions on the key alone (`x.Id == id`) are left out: that is what `Find` is for. Repeated queries do not count as duplicated lines and never fail `--fail-on-copies`: they are advice. In the [dashboard](review.md) they are the *query* kind, with a chart of the tables queried the same way.
