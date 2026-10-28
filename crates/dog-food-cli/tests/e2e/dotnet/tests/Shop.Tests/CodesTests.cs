namespace Shop.Tests;

public class CodesCountTests
{
    [Fact]
    public void There_are_three_codes() => Assert.Equal(3, Codes.All.Count);

    [Fact]
    public void Squares_are_positive() => Assert.True(Codes.Square(3) > 0);
}

public class CodesValueTests
{
    [Fact]
    public void Codes_go_up_in_tens() => Assert.Equal(10, Codes.All[1]);

    [Fact]
    public void Three_squared_is_nine() => Assert.Equal(9, Codes.Square(3));
}
