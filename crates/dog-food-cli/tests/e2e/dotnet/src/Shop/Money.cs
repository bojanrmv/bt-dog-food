namespace Shop;

public readonly record struct Money(decimal Amount)
{
    public static Money operator +(Money a, Money b) => new(a.Amount + b.Amount);

    // Money has no `-`, which only the compiler can tell: that mutant does not compile.
    public static Money Total(Money a, Money b)
    {
        return a + b;
    }
}
