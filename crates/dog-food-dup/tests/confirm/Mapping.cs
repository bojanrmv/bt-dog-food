// Fixture for --confirm: groups that running them confirms, one it can not
// run, and a constructor that only stores what it is given.
namespace Shop.Confirm;

public enum Tier { Basic, Plus, Pro }

public record Member
{
    public required string Name { get; init; }
    public Tier Tier { get; init; }
    public decimal Spent { get; init; }
}

public record Badge
{
    public required string Label { get; init; }
    public int Level { get; init; }
}

public sealed class Price(decimal amount)
{
    readonly decimal amount = amount;

    public decimal Amount => amount;
}

public static class Badges
{
    // The same badge, from a switch and from ifs: running them confirms it.
    public static Badge FromSwitch(Member member) => new()
    {
        Label = member.Name.Trim(),
        Level = member.Tier switch
        {
            Tier.Pro => 3,
            Tier.Plus => 2,
            _ => member.Spent >= 500m ? 2 : 1,
        },
    };

    public static Badge FromIfs(Member member)
    {
        var level = 1;
        if (member.Tier == Tier.Pro)
        {
            level = 3;
        }
        else if (member.Tier == Tier.Plus || member.Spent >= 500m)
        {
            level = 2;
        }
        return new Badge { Label = member.Name.Trim(), Level = level };
    }
}

public static class Prices
{
    public static Price Discounted(Member member, decimal amount) =>
        new(member.Tier == Tier.Pro ? amount * 0.9m : amount);

    public static Price Reduced(Member member, decimal amount)
    {
        if (member.Tier != Tier.Pro)
        {
            return new Price(amount);
        }
        return new Price(amount * 0.9m);
    }
}

public static class Stamps
{
    // Reads the clock: not run.
    public static string Today(Member member) =>
        member.Name + " " + System.DateTime.Now.ToString("yyyy") + (member.Tier == Tier.Pro ? " pro" : "") + Audit.Write(member.Name);

    public static string Now(Member member) =>
        member.Name + " " + System.DateTime.Now.ToString("yyyy") + (member.Tier != Tier.Pro ? "" : " pro") + Audit.Write(member.Name);
}

public static class Audit
{
    public static string Write(string name)
    {
        System.IO.File.AppendAllText("audit.log", name);
        return "";
    }
}
