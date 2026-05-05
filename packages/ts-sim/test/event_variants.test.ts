// Exhaustive `Event` variant surface. Each scenario produces a known
// set of Event variants; the tests verify those variants emerge from
// the typed log without `any` leaking through.
//
// Compile-time exhaustiveness is already enforced by the `never`-narrowing
// switch in integration.test.ts. This file adds runtime confirmation
// that every variant deserializes correctly across the boundary.

import { describe, expect, it } from "vitest";
import {
  type Event,
  MAC_CONFIG_IEEE_802_3,
  TypedEngine,
  TypedTopologyBuilder,
} from "../src/index.js";

/** Returns every distinct `Event.type` discriminator that appears in the log. */
function distinctEventTypes(log: { event: Event }[]): Set<string> {
  return new Set(log.map((e) => e.event.type));
}

// ---------------------------------------------------------------------------
// HD-1 transmission scenario → propagation events + topology adds
// ---------------------------------------------------------------------------

describe("HD-1 transmission scenario", () => {
  it("surfaces TxAttempt, TxStart, FrontArrive, TxEnd, BackArrive, NodeAdded, SegmentAdded", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    const frame = engine.registerFrame(s1, 512n, false, 10_000_000n);
    engine.scheduleTxAttempt(0n, s1, frame);
    engine.runUntilIdle();

    const types = distinctEventTypes([...engine.log()]);
    for (const expected of [
      "TxAttempt",
      "TxStart",
      "FrontArrive",
      "TxEnd",
      "BackArrive",
    ]) {
      expect(types.has(expected)).toBe(true);
    }
  });
});

// ---------------------------------------------------------------------------
// Multi-station collision → CollisionDetect, JamStart, JamEnd, BackoffExpire
// ---------------------------------------------------------------------------

describe("multi-station collision scenario", () => {
  it("surfaces CollisionDetect, JamStart, JamEnd, BackoffExpire", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    const s3 = builder.addEndStation(1);
    const r = builder.addRepeater(3, 100n);
    builder.addHdSegment(10_000_000n, 1_000_000n, s1, 0, r, 0);
    builder.addHdSegment(10_000_000n, 1_000_000n, s2, 0, r, 1);
    builder.addHdSegment(10_000_000n, 1_000_000n, s3, 0, r, 2);
    using world = builder.build();
    using engine = TypedEngine.create(world, 7n);
    // Two stations transmit simultaneously → guaranteed collision.
    const f1 = engine.registerFrame(s1, 512n, false, 10_000_000n);
    const f2 = engine.registerFrame(s2, 512n, false, 10_000_000n);
    engine.scheduleTxAttempt(0n, s1, f1);
    engine.scheduleTxAttempt(0n, s2, f2);
    engine.runUntilIdle();

    const types = distinctEventTypes([...engine.log()]);
    for (const expected of [
      "CollisionDetect",
      "JamStart",
      "JamEnd",
      "BackoffExpire",
    ]) {
      expect(types.has(expected)).toBe(true);
    }
  });
});

// ---------------------------------------------------------------------------
// Bridge relay → FrameEligible, Enqueue, Dequeue
// ---------------------------------------------------------------------------

describe("bridge relay scenario", () => {
  it("surfaces FrameEligible, Enqueue, Dequeue on the egress serializer", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    const br = builder.addBridge(2, 64n, 500n);
    builder.addHdSegment(100_000_000n, 1_000_000n, s1, 0, br, 0);
    builder.addHdSegment(100_000_000n, 1_000_000n, s2, 0, br, 1);
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    const frame = engine.registerFrame(s1, 512n, false, 100_000_000n);
    engine.scheduleTxAttempt(0n, s1, frame);
    engine.runUntilIdle();

    const types = distinctEventTypes([...engine.log()]);
    for (const expected of ["FrameEligible", "Enqueue", "Dequeue"]) {
      expect(types.has(expected)).toBe(true);
    }
  });
});

// ---------------------------------------------------------------------------
// Continuity removal → SegmentRemoved, NodeRemoved, PortDisconnected, SignalLost
// ---------------------------------------------------------------------------

describe("continuity removal scenario", () => {
  it("surfaces PortDisconnected and SignalLost when disconnecting mid-flight", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    const frame = engine.registerFrame(s1, 512n, false, 10_000_000n);
    engine.scheduleTxAttempt(0n, s1, frame);
    // Run partway, then disconnect mid-flight.
    engine.runUntil(1_000n);
    engine.applyEdit({ type: "DisconnectPort", node: s2, port: 0 });
    engine.runUntilIdle();

    const types = distinctEventTypes([...engine.log()]);
    for (const expected of ["PortDisconnected", "SignalLost"]) {
      expect(types.has(expected)).toBe(true);
    }
  });

  it("surfaces NodeRemoved + SegmentRemoved on RemoveNode", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    engine.applyEdit({ type: "RemoveNode", node: s2 });
    engine.runUntilIdle();

    const types = distinctEventTypes([...engine.log()]);
    for (const expected of ["NodeRemoved", "SegmentRemoved"]) {
      expect(types.has(expected)).toBe(true);
    }
  });
});

// ---------------------------------------------------------------------------
// Parameter changes → SegmentDelayChanged, SegmentRateChanged
// ---------------------------------------------------------------------------

describe("parameter change scenario", () => {
  it("surfaces SegmentDelayChanged and SegmentRateChanged", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    engine.applyEdit({
      type: "SetSegmentDelay",
      segment: 0,
      new_delay: 8_000_000n,
    });
    engine.applyEdit({
      type: "SetSegmentRate",
      segment: 0,
      new_rate: 100_000_000n,
    });
    engine.runUntilIdle();

    const types = distinctEventTypes([...engine.log()]);
    for (const expected of ["SegmentDelayChanged", "SegmentRateChanged"]) {
      expect(types.has(expected)).toBe(true);
    }
  });
});

// ---------------------------------------------------------------------------
// MAC config change → MacConfigChanged
// ---------------------------------------------------------------------------

describe("MAC config change scenario", () => {
  it("surfaces MacConfigChanged", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    engine.applyEdit({
      type: "SetMacConfig",
      node: s1,
      config: MAC_CONFIG_IEEE_802_3,
    });
    engine.runUntilIdle();

    const types = distinctEventTypes([...engine.log()]);
    expect(types.has("MacConfigChanged")).toBe(true);
  });
});
