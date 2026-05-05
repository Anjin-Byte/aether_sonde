import { defineConfig } from "vitest/config";
import wasm from "vite-plugin-wasm";
import topLevelAwait from "vite-plugin-top-level-await";

// Vite's default loader doesn't handle the bundler-target wasm-pack
// output (which uses the ESM integration proposal). The two plugins
// below add the standard support.
export default defineConfig({
  plugins: [wasm(), topLevelAwait()],
});
