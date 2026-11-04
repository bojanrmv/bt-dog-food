namespace Calc;

public static class Calc
{
    public static readonly int[] Steps = Build();

    private static int[] Build() => new[] { 1 * 10, 2 * 10 };

    public static int Add(int a, int b) => a + b;

    public static bool IsAdult(int age) => age >= 18;

    public static int Count(long distance, long step)
    {
        var count = 0;
        while (distance > 0)
        {
            distance -= step;
            count++;
        }
        return count;
    }

    /// Ends the process for a negative count: a mutant that lets zero
    /// through takes the test host down with it.
    public static int Guarded(int n)
    {
        if (n < 0)
        {
            System.Diagnostics.Process.GetCurrentProcess().Kill();
        }
        return n;
    }

    // A value cached once per process in ordinary code (no static
    // constructor): a mutant that fills it in one test is still there for
    // the next test's mutant in a warm test process. The kill check keeps
    // that from counting as a kill of the wrong mutant.
    private static int doubled = -1;

    public static int Doubled(int x)
    {
        if (doubled < 0)
        {
            doubled = x * 2;
        }
        return doubled;
    }

    public static int Scaled(int x)
    {
        var y = Doubled(x);
        return y >= 0 ? y + 1 : 0;
    }
}
