// ESLint flat config for `@aether-sonde/sim`.
//
// Layered on top of strict TypeScript: tsc owns soundness, ESLint owns
// stylistic and best-practice rules that the type system can't catch
// (consistent type imports, unused vars, prefer-const, etc.).
//
// Type-aware rules (no-floating-promises, no-misused-promises) are
// deferred until the API matures — adding them requires a tsconfig
// reachable from every linted file, including config files at the
// package root.

import eslint from "@eslint/js";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "dist/**",
      "node_modules/**",
      // wasm-pack output: not ours to lint.
      "../../crates/wasm/pkg/**",
    ],
  },
  eslint.configs.recommended,
  ...tseslint.configs.recommended,
  ...tseslint.configs.stylistic,
  {
    files: ["test/**/*.ts"],
    rules: {
      // Test patterns commonly use post-`expect` non-null assertions:
      //   expect(x).toBeDefined();
      //   expect(x!.foo).toBe(bar);
      "@typescript-eslint/no-non-null-assertion": "off",
      // Tests construct objects literally; explicit-any is rare but
      // sometimes used in fixture builders.
      "@typescript-eslint/no-explicit-any": "off",
    },
  },
);
