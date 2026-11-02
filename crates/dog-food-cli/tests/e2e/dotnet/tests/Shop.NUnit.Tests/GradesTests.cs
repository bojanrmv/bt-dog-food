using NUnit.Framework;

namespace Shop.NUnit.Tests;

public class GradesTests
{
    [TestCase(50, "pass")]
    [TestCase(49, "fail")]
    public void Scores_get_their_grade(int score, string grade) =>
        Assert.That(Grades.Of(score), Is.EqualTo(grade));
}
