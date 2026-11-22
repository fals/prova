namespace Calc;

public static class Primes
{
    /// <summary>Counts primes below n by trial division over odd divisors.</summary>
    public static int CountPrimes(int n)
    {
        int count = 0;
        for (int k = 2; k < n; k++)
        {
            if (IsPrime(k))
            {
                count++;
            }
        }
        return count;
    }

    private static bool IsPrime(int k)
    {
        if (k < 2)
        {
            return false;
        }
        if (k % 2 == 0)
        {
            return k == 2;
        }
        for (int d = 3; d * d <= k; d += 2)
        {
            if (k % d == 0)
            {
                return false;
            }
        }
        return true;
    }
}
