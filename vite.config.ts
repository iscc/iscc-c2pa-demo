import { defineConfig } from "vite";

// Port derived from the project name so parallel projects do not collide.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  clearScreen: false,
  server: {
    port: 43172,
    strictPort: true,
    host: host || false,
    watch: { ignored: ["**/src-tauri/**"] },
  },
  build: { target: "es2022", minify: true, sourcemap: false },
});
