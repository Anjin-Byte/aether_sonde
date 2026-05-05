// Determinism via the typed adapter: same `(spec, seed, schedule, edits)` →
// JSON-stringified log is character-identical.

import { describe, expect, it } from "vitest";
import {
  TypedEngine,
  TypedTopologyBuilder,
} from "../src/index.js";

function runHd1(seed: bigint): string {
  using builder = TypedTopologyBuilder.create();
  const s1 = builder.addEndStation(1);
  const s2 = builder.addEndStation(1);
  builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
  const world = builder.build();
  using engine = TypedEngine.create(world, seed);
  const frame = engine.registerFrame(s1, 512n, false, 10_000_000n);
  engine.scheduleTxAttempt(0n, s1, frame);
  engine.runUntilIdle();
  // BigInt is not JSON-serializable directly; convert to strings.
  return JSON.stringify(engine.logSnapshot(), (_k, v) =>
    typeof v === "bigint" ? `${v}` : v,
  );
}

describe("determinism via the typed adapter", () => {
  it("same seed produces byte-identical JSON-stringified log", () => {
    const a = runHd1(42n);
    const b = runHd1(42n);
    expect(a).toBe(b);
  });

  it("BEB-free FD-shape scenario is seed-independent", () => {
    // Single-tx FD scenarios never trigger BEB, so all seeds agree.
    function runFd(seed: bigint): string {
      using builder = TypedTopologyBuilder.create();
      const s1 = builder.addEndStation(1);
      const s2 = builder.addEndStation(1);
      builder.addFdSegment(1_000_000_000n, 100n, s1, 0, s2, 0);
      const world = builder.build();
      using engine = TypedEngine.create(world, seed);
      const frame = engine.registerFrame(s1, 512n, false, 1_000_000_000n);
      engine.scheduleTxAttempt(0n, s1, frame);
      engine.runUntilIdle();
      return JSON.stringify(engine.logSnapshot(), (_k, v) =>
        typeof v === "bigint" ? `${v}` : v,
      );
    }
    const baseline = runFd(0n);
    for (let s = 1n; s < 5n; s++) {
      expect(runFd(s)).toBe(baseline);
    }
  });
});
