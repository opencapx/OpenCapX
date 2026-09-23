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
  },
});
