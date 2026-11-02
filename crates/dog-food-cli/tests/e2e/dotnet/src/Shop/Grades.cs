namespace Shop;

/// Tested only by the NUnit project, whose tests the mutant switches must
/// tell apart.
public static class Grades
{
    public static string Of(int score) => score >= 50 ? "pass" : "fail";
}
