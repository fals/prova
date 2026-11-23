import { defineConfig } from "vitest/config";

// No per-test timeout: hangs must stay hangs so prova can detect them.
export default defineConfig({ test: { testTimeout: 24 * 60 * 60 * 1000 } });
