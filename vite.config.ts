import { defineConfig } from "vite";

export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: "es2021",
    rollupOptions: {
      input: { overlay: "index.html", settings: "settings.html" },
    },
  },
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
    // Coverage is a report, not a gate: the DOM-heavy modules (settings/*, overlay)
    // are exercised through the app, not jsdom, so their line counts drag the total
    // down by design. CI (coverage.yml) uploads lcov; numbers live in the job summary.
    coverage: {
      provider: "v8",
      reporter: ["text-summary", "lcov", "json-summary"],
      reportsDirectory: "coverage",
      include: ["src/**"],
      exclude: ["src/**/*.test.ts", "src/locales/**"],
    },
  },
});
