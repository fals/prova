using System.Diagnostics;
using System.Threading;
using Xunit;

namespace Calc;

public class CalcTests
{
    [Fact]
    public void Adds() => Assert.Equal(5, Calculator.Add(2, 3));

    [Fact]
    public void Divides() => Assert.Equal(4, Calculator.Div(8, 2));

    [Fact]
    public void FlakyClock() =>
        Assert.True(Stopwatch.GetTimestamp() / 1000 % 3 != 0, "clock tick landed on a bad boundary");

    [Fact]
    public void SlowIntegration() => Thread.Sleep(1500);

    [Fact]
    public void HangsWhenAsked()
    {
        if (System.Environment.GetEnvironmentVariable("HANG") != null)
        {
            Thread.Sleep(Timeout.Infinite);
        }
    }
}
