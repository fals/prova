import { bench, describe } from "vitest";
import { countPrimes } from "./primes";

describe("primes", () => {
  bench("countPrimes", () => {
    if (countPrimes(500000) !== 41538) throw new Error("wrong prime count");
  });
});
