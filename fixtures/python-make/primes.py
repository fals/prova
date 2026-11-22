def count_primes(n):
    """Number of primes below n, by trial division over odd divisors."""
    if n <= 2:
        return 0
    count = 1  # the prime 2
    for k in range(3, n, 2):
        d = 3
        prime = True
        while d * d <= k:
            if k % d == 0:
                prime = False
                break
            d += 2
        if prime:
            count += 1
    return count
