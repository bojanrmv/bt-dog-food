using System.Threading.Tasks;
using Xunit;

namespace Calc.Tests;

public class CalcTests
{
    [Fact]
    public void Adds() => Assert.Equal(5, Calc.Add(2, 3));

    [Theory]
    [InlineData(18)]
    [InlineData(30)]
    public void Adults(int age) => Assert.True(Calc.IsAdult(age));

    [Fact]
    public void Steps_go_up_in_tens() => Assert.Equal(20, Calc.Steps[1]);

    [Fact]
    public void Thirty_in_tens_is_three() => Assert.Equal(3, Calc.Count(30, 10));

    [Fact]
    public void Zero_is_guarded_through() => Assert.Equal(0, Calc.Guarded(0));

    [Fact]
    public void Two_doubled_is_four() => Assert.Equal(4, Calc.Doubled(2));

    [Fact]
    public void Two_scaled_is_five() => Assert.Equal(5, Calc.Scaled(2));

    // The running test follows the work onto another thread.
    [Fact]
    public async Task Later_on_another_thread() => Assert.Equal(1, await Task.Run(() => Calc.Later(2)));

    public class Nested
    {
        [Fact]
        public void Halves() => Assert.Equal(2, Calc.Half(4));
    }
}

public class Setup
{
    public int Value { get; } = Calc.Fixed(1);
}

public class FixtureTests(Setup setup) : IClassFixture<Setup>
{
    [Fact]
    public void Built_once() => Assert.Equal(2, setup.Value);
}
