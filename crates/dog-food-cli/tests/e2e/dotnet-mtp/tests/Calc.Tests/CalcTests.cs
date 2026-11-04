using Microsoft.VisualStudio.TestTools.UnitTesting;

namespace Calc.Tests;

[TestClass]
public class CalcTests
{
    [TestMethod]
    public void Adds() => Assert.AreEqual(5, Calc.Add(2, 3));

    [TestMethod]
    [DataRow(18)]
    [DataRow(30)]
    public void Adults(int age) => Assert.IsTrue(Calc.IsAdult(age));

    [TestMethod]
    public void Steps_go_up_in_tens() => Assert.AreEqual(20, Calc.Steps[1]);

    [TestMethod]
    public void Thirty_in_tens_is_three() => Assert.AreEqual(3, Calc.Count(30, 10));

    [TestMethod]
    public void Zero_is_guarded_through() => Assert.AreEqual(0, Calc.Guarded(0));

    [TestMethod]
    public void Two_doubled_is_four() => Assert.AreEqual(4, Calc.Doubled(2));

    [TestMethod]
    public void Two_scaled_is_five() => Assert.AreEqual(5, Calc.Scaled(2));
}
