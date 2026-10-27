namespace Shop.Tests;

public class CodesCountTests
{
    [Fact]
    public void There_are_three_codes() => Assert.Equal(3, Codes.All.Count);
}

public class CodesValueTests
{
    [Fact]
    public void Codes_go_up_in_tens() => Assert.Equal(10, Codes.All[1]);
}
