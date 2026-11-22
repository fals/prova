using BenchmarkDotNet.Attributes;
using BenchmarkDotNet.Configs;
using BenchmarkDotNet.Engines;
using BenchmarkDotNet.Jobs;
using BenchmarkDotNet.Reports;
using BenchmarkDotNet.Running;
using BenchmarkDotNet.Toolchains.InProcess.Emit;
using Perfolizer.Horology;

namespace Calc.Bench;

/// <summary>Short in-process job: 1 launch, 1 warmup, 3 measured iterations of ~200 ms, results in ms.</summary>
public class FastConfig : ManualConfig
{
    public FastConfig()
    {
        AddJob(Job.Default
            .WithToolchain(InProcessEmitToolchain.Instance)
            .WithStrategy(RunStrategy.Throughput)
            .WithLaunchCount(1)
            .WithWarmupCount(1)
            .WithIterationCount(3)
            .WithIterationTime(TimeInterval.FromMilliseconds(200))
            .WithId("Fast"));
        // Fixed unit so the summary table always reports milliseconds.
        SummaryStyle = SummaryStyle.Default.WithTimeUnit(TimeUnit.Millisecond);
    }
}

[Config(typeof(FastConfig))]
public class PrimesBench
{
    private const int N = 500_000;

    [Benchmark]
    public int CountPrimes()
    {
        int result = Primes.CountPrimes(N);
        if (result != 41538)
        {
            throw new System.InvalidOperationException($"CountPrimes({N}) = {result}, expected 41538");
        }
        return result;
    }
}

public static class Program
{
    public static void Main(string[] args) =>
        BenchmarkSwitcher.FromAssembly(typeof(Program).Assembly).Run(args);
}
