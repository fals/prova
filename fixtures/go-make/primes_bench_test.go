package calc

import "testing"

func BenchmarkCountPrimes(b *testing.B) {
	for i := 0; i < b.N; i++ {
		if got := CountPrimes(500000); got != 41538 {
			b.Fatalf("CountPrimes(500000) = %d, want 41538", got)
		}
	}
}
