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
}
