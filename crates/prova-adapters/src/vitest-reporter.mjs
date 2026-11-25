// prova's Vitest reporter (opt-in recipe `vitest-stream`). Appends one JSON
// line per test event to the file named by PROVA_STREAM, in libtest's JSON
// shape plus a `file` field, so prova sees every test live at level 3.
// Load it next to the default reporter:
//   vitest run --reporter=default --reporter="$PROVA_VITEST_REPORTER"
import { appendFileSync, existsSync } from "node:fs";
import { relative } from "node:path";
import { fileURLToPath } from "node:url";

export default class ProvaReporter {
  onInit(vitest) {
    this.root = vitest.config.root;
    this.out = process.env.PROVA_STREAM;
    this.started = new Set();
    this.order = new Map(); // module id -> its tests, in run order
    this.pending = new Map(); // test id -> timer of an inferred start
    this.addWorkerHooks(vitest);
  }

  // Vitest's worker sends its task updates through a throttle that can lose
  // the trailing call (3.2.4): a start or an early failure then reaches this
  // reporter only with the next update, seconds late. The setup file
  // (prova-vitest-setup.mjs, next to this file) writes starts and results
  // from inside the worker instead. Any other Vitest, or a failure here,
  // leaves things as they were: the reporter's own events remain.
  addWorkerHooks(vitest) {
    try {
      if (!this.out) return;
      const setup = fileURLToPath(new URL("./prova-vitest-setup.mjs", import.meta.url));
      if (!existsSync(setup)) return;
      process.env.PROVA_VITEST_ROOT = this.root;
      const configs = new Set([vitest.config, ...(vitest.projects ?? []).map((p) => p.config)]);
      for (const config of configs) {
        if (!config || !Array.isArray(config.setupFiles)) continue;
        if (!config.setupFiles.includes(setup)) config.setupFiles.push(setup);
      }
    } catch {
      // Not a Vitest we know: keep the reporter's events only.
    }
  }

  write(event) {
    if (this.out) appendFileSync(this.out, JSON.stringify(event) + "\n");
  }

  base(testCase) {
    return {
      type: "test",
      name: testCase.fullName,
      file: relative(this.root, testCase.module.moduleId),
    };
  }

  start(testCase) {
    if (this.started.has(testCase.id)) return;
    this.started.add(testCase.id);
    this.write({ ...this.base(testCase), event: "started" });
  }

  onTestCaseReady(testCase) {
    this.start(testCase);
  }

  // Vitest batches "ready" updates from the worker: a test that hangs may
  // never have its own reach the reporter. Tests of one file run in order,
  // so when one finishes the next one has started (prova round 9). Results
  // are batched too, so that start is announced only if the test's own
  // result has not come 5 s later (round 11: a false SLOW on a 0 ms test).
  infer(testCase) {
    if (this.started.has(testCase.id) || this.pending.has(testCase.id)) return;
    const timer = setTimeout(() => {
      this.pending.delete(testCase.id);
      this.start(testCase);
    }, 5000);
    timer.unref?.();
    this.pending.set(testCase.id, timer);
  }
  onTestModuleCollected(testModule) {
    const tests = [...testModule.children.allTests()].filter((t) => t.options?.mode !== "skip" && t.options?.mode !== "todo");
    this.order.set(testModule.moduleId, tests);
    if (tests.length && !tests[0].options?.concurrent) this.infer(tests[0]);
  }

  next(testCase) {
    const tests = this.order.get(testCase.module.moduleId) ?? [];
    const i = tests.findIndex((t) => t.id === testCase.id);
    const n = i >= 0 ? tests[i + 1] : undefined;
    if (n && !n.options?.concurrent) this.infer(n);
  }

  onTestCaseResult(testCase) {
    clearTimeout(this.pending.get(testCase.id));
    this.pending.delete(testCase.id);
    this.started.add(testCase.id);
    this.next(testCase);
    const result = testCase.result();
    const event = { passed: "ok", failed: "failed", skipped: "ignored" }[result.state];
    if (!event) return;
    const diagnostic = testCase.diagnostic();
    const errors = result.errors ?? [];
    const message = errors.map((e) => e.message).join("\n");
    const stack = errors.map((e) => e.stack).filter(Boolean).join("\n");
    this.write({
      ...this.base(testCase),
      event,
      exec_time: diagnostic ? diagnostic.duration / 1000 : undefined,
      message: message || undefined,
      stack: stack || undefined,
    });
  }

  // A test file that fails to load (an import error, a syntax error) has no
  // test to fail: Vitest reports the file itself as failed, and the default
  // reporter prints "FAIL file [ file ]". It is one errored entry, named
  // `file::(file)` like a Go package that fails to build.
  onFinished(files) {
    for (const file of files ?? []) {
      if (file.result?.state !== "fail") continue;
      const errors = file.result.errors ?? [];
      if (errors.length === 0 && hasFailedTest(file)) continue;
      const message = errors.map((e) => e.message).join("\n") || "the test file failed to run";
      const stack = errors.map((e) => e.stack).filter(Boolean).join("\n");
      this.write({
        type: "test",
        name: "(file)",
        file: relative(this.root, file.filepath),
        event: "failed",
        message,
        stack: stack || undefined,
      });
    }
  }
}

function hasFailedTest(task) {
  if (task.type === "test" || task.type === "custom") return task.result?.state === "fail";
  return (task.tasks ?? []).some(hasFailedTest);
}
