namespace Shop.Tests;

public class GateTests
{
    [Fact]
    public void An_open_gate_lets_through() => Assert.True(Gate.Passes(false));
}
