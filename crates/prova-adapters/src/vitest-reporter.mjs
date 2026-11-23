// prova's Vitest reporter (opt-in recipe `vitest-stream`). Appends one JSON
// line per test event to the file named by PROVA_STREAM, in libtest's JSON
// shape plus a `file` field, so prova sees every test live at level 3.
// Load it next to the default reporter:
//   vitest run --reporter=default --reporter="$PROVA_VITEST_REPORTER"
import { appendFileSync } from "node:fs";
import { relative } from "node:path";

export default class ProvaReporter {
  onInit(vitest) {
    this.root = vitest.config.root;
    this.out = process.env.PROVA_STREAM;
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

  onTestCaseReady(testCase) {
    this.write({ ...this.base(testCase), event: "started" });
  }

  onTestCaseResult(testCase) {
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
}
