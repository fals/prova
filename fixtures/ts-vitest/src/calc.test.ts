import { describe, expect, it } from "vitest";
import { add, div } from "./calc";

describe("calc", () => {
  it("adds", () => {
    expect(add(2, 3)).toBe(5);
  });

  it("divides", () => {
    expect(div(8, 2)).toBe(4);
  });

  it("flaky clock", () => {
    const micros = Math.floor(performance.now() * 1000);
    expect(micros % 3, "clock tick landed on a bad boundary").not.toBe(0);
  });

  it("slow integration", async () => {
    await new Promise((r) => setTimeout(r, 1500));
  });

  it("hangs when asked", async () => {
    if (process.env.HANG) {
      await new Promise(() => {});
    }
  });
});
