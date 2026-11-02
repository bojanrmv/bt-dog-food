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
}
