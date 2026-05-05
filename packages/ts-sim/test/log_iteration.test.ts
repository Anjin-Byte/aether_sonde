// Frontend-pattern tests: realistic ways a DOM frontend will iterate
// over the typed log. These don't add behavioral oracles — they
// validate that the typed adapter's `LoggedEvent[]` shape supports the
// iteration patterns frontend code will exercise (filter, narrow,
// pluck, group, paginate).

import { describe, expect, it } from "vitest";
import {
  type LoggedEvent,
  type Phase,
  type Signal,
  TypedEngine,
  TypedTopologyBuilder,
} from "../src/index.js";

/** Run a 3-station collision scenario; returns the log for iteration. */
function collisionLog(): readonly LoggedEvent[] {
  using builder = TypedTopologyBuilder.create();
  const s1 = builder.addEndStation(1);
  const s2 = builder.addEndStation(1);
  const s3 = builder.addEndStation(1);
  const r = builder.addRepeater(3, 100n);
  builder.addHdSegment(10_000_000n, 1_000_000n, s1, 0, r, 0);
  builder.addHdSegment(10_000_000n, 1_000_000n, s2, 0, r, 1);
  builder.addHdSegment(10_000_000n, 1_000_000n, s3, 0, r, 2);
  using world = builder.build();
  using engine = TypedEngine.create(world, 0n);
  const f1 = engine.registerFrame(s1, 512n, false, 10_000_000n);
  const f2 = engine.registerFrame(s2, 512n, false, 10_000_000n);
  engine.scheduleTxAttempt(0n, s1, f1);
  engine.scheduleTxAttempt(0n, s2, f2);
  engine.runUntilIdle();
  return engine.log();
}

// ---------------------------------------------------------------------------
// Filter by event type — narrowing inside a callback
// ---------------------------------------------------------------------------

describe("filter by event type", () => {
  it("narrows the predicate result to the variant's payload shape", () => {
    const log = collisionLog();
    const collisions = log.filter(
      (e): e is LoggedEvent & { event: { type: "CollisionDetect" } } =>
        e.event.type === "CollisionDetect",
    );
    // Inside this slice, e.event.type is "CollisionDetect" — the user-defined
    // type guard narrowed both the event tag and the surrounding key.
    expect(collisions.length).toBeGreaterThan(0);
    for (const entry of collisions) {
      expect(entry.event.type).toBe("CollisionDetect");
      expect(typeof entry.key.time).toBe("bigint");
    }
  });
});

// ---------------------------------------------------------------------------
// Pluck signal-bearing events via switch narrowing
// ---------------------------------------------------------------------------

describe("pluck signal-bearing events", () => {
  it("extracts Signal from variants that carry one", () => {
    const log = collisionLog();
    const signals: Signal[] = [];
    for (const entry of log) {
      switch (entry.event.type) {
        case "TxStart":
        case "TxEnd":
        case "FrontArrive":
        case "BackArrive":
        case "CollisionDetect":
        case "SignalLost":
          signals.push(entry.event.signal);
          break;
        default:
          break;
      }
    }
    expect(signals.length).toBeGreaterThan(0);
    // Every plucked signal has the expected shape.
    for (const s of signals) {
      expect(typeof s.source).toBe("number");
      expect(typeof s.t0).toBe("bigint");
      expect(typeof s.duration).toBe("bigint");
      expect(s.kind === "Frame" || s.kind === "Jam").toBe(true);
    }
  });
});

// ---------------------------------------------------------------------------
// Count by phase — diagnostics-style grouping
// ---------------------------------------------------------------------------

describe("count by phase", () => {
  it("groups log entries by phase deterministically", () => {
    const log = collisionLog();
    const counts: Record<Phase, number> = {
      Release: 0,
      Assertion: 0,
      Reaction: 0,
      LocalDecision: 0,
    };
    for (const entry of log) {
      counts[entry.key.phase] += 1;
    }
    // Every phase has at least one entry in this collision scenario.
    expect(counts.LocalDecision).toBeGreaterThan(0); // TxAttempt, TxStart, topology events
    expect(counts.Assertion).toBeGreaterThan(0); // FrontArrive
    expect(counts.Release).toBeGreaterThan(0); // TxEnd, BackArrive, JamEnd
    expect(counts.Reaction).toBeGreaterThan(0); // CollisionDetect, JamStart
  });
});

// ---------------------------------------------------------------------------
// Find first event matching a predicate
// ---------------------------------------------------------------------------

describe("find first matching event", () => {
  it("locates the earliest CollisionDetect at any node", () => {
    const log = collisionLog();
    const first = log.find((e) => e.event.type === "CollisionDetect");
    expect(first).toBeDefined();
    if (first?.event.type === "CollisionDetect") {
      expect(typeof first.event.node).toBe("number");
      expect(first.key.time).toBeGreaterThan(0n);
    }
  });
});

// ---------------------------------------------------------------------------
// Paginate
// ---------------------------------------------------------------------------

describe("paginate the log", () => {
  it("supports slicing for chunked rendering", () => {
    const log = collisionLog();
    const pageSize = 10;
    const pages: (readonly LoggedEvent[])[] = [];
    for (let i = 0; i < log.length; i += pageSize) {
      pages.push(log.slice(i, i + pageSize));
    }
    // At least one page; concatenated pages reconstruct the log.
    expect(pages.length).toBeGreaterThan(0);
    const concatenated = pages.flat();
    expect(concatenated.length).toBe(log.length);
    expect(concatenated[0]).toEqual(log[0]);
  });
});
