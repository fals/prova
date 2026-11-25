// prova's Vitest worker hooks (opt-in recipe `vitest-stream`), added to the
// project's setupFiles by prova's reporter. They run inside the worker and
// append a start and a final result per test straight to PROVA_STREAM.
// Vitest 3 sends the worker's task updates through a throttle that can lose
// its trailing call, so the reporter hears of a start or an early failure
// only at the next update; this file does not depend on it. The reporter
// still writes its own events (messages, stacks, skipped tests, files that
// failed to load); prova keeps the first result of a test and ignores a
// repeat of it.
import * as vitest from "vitest";
import { appendFileSync } from "node:fs";
import { relative } from "node:path";

const out = process.env.PROVA_STREAM;
const root = process.env.PROVA_VITEST_ROOT || process.cwd();

function write(event) {
  try {
    appendFileSync(out, JSON.stringify(event) + "\n");
  } catch {
    // The stream is a courtesy; a test never fails because of it.
  }
}

// "describe > nested > test", as the reporter's `fullName` gives it.
function fullName(task) {
  const names = [task.name];
  for (let s = task.suite; s && s !== task.file && s.name; s = s.suite) names.unshift(s.name);
  return names.join(" > ");
}

if (out && typeof vitest.beforeEach === "function") {
  vitest.beforeEach((ctx) => {
    const task = ctx?.task;
    const filepath = task?.file?.filepath;
    if (!filepath) return;
    const base = { type: "test", name: fullName(task), file: relative(root, filepath) };
    const began = performance.now();
    write({ ...base, event: "started" });
    const onFinished = ctx.onTestFinished ?? vitest.onTestFinished;
    if (typeof onFinished !== "function") return;
    onFinished(() => {
      // A retried, repeated or inverted (`test.fails`) test has no result
      // of its own here: the reporter reports its final one.
      if ((task.retry ?? 0) > 0 || (task.repeats ?? 0) > 0 || task.fails) return;
      const state = task.result?.state;
      if (state !== "pass" && state !== "fail") return;
      const errors = task.result?.errors ?? [];
      write({
        ...base,
        event: state === "pass" ? "ok" : "failed",
        exec_time: (performance.now() - began) / 1000,
        message: errors.map((e) => e.message).join("\n") || undefined,
      });
    });
  });
}
