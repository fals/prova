from primes import count_primes


def test_count_primes(benchmark):
    assert benchmark(count_primes, 50000) == 5133
