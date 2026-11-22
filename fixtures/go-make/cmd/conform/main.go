// Command conform reads the Lean specification's answers ("n count" per
// line, from `lake exe reference`) on stdin and checks that CountPrimes
// gives the same answer for every n. Exits 1 at the first difference.
package main

import (
	"bufio"
	"fmt"
	"os"

	"example.com/calc"
)

func main() {
	in := bufio.NewScanner(os.Stdin)
	checked := 0
	for in.Scan() {
		var n, want int
		if _, err := fmt.Sscan(in.Text(), &n, &want); err != nil {
			fmt.Fprintf(os.Stderr, "conform: bad line %q: %v\n", in.Text(), err)
			os.Exit(2)
		}
		if got := calc.CountPrimes(n); got != want {
			fmt.Printf("conform: CountPrimes(%d) = %d, specification says %d\n", n, got, want)
			os.Exit(1)
		}
		checked++
	}
	if checked == 0 {
		fmt.Fprintln(os.Stderr, "conform: no reference answers on stdin")
		os.Exit(2)
	}
	fmt.Printf("conform: %d answers match the specification\n", checked)
}
