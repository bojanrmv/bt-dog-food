namespace Shop;

public static class Gate
{
    /// Waits until the gate opens. One that never opens is a test that never
    /// ends, and one that uses no processor while it waits.
    public static bool Passes(bool closed)
    {
        using var gate = new System.Threading.ManualResetEventSlim(!closed);
        gate.Wait();
        return true;
    }
}
