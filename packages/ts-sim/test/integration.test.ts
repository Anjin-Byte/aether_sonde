// Integration tests: build a topology, run a simulation, and verify
// the typed log matches the native HD-1 oracle.

import { describe, expect, it } from "vitest";
import {
  type Event,
  TypedEngine,
  TypedTopologyBuilder,
  carrierSense,
} from "../src/index.js";

describe("TypedTopologyBuilder", () => {
  it("builds a 2-station HD pair with the expected counts", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
    using world = builder.build();
    expect(world.nodeCount()).toBe(2);
    expect(world.segmentCount()).toBe(1);
    expect(world.collisionResourceCount()).toBe(1);
  });
});

describe("HD-1 oracle via typed adapter", () => {
  it("produces exactly 5 log entries with the expected timestamps", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
    const world = builder.build();
    using engine = TypedEngine.create(world, 1n);

    const frame = engine.registerFrame(s1, 512n, false, 10_000_000n);
    engine.scheduleTxAttempt(0n, s1, frame);
    engine.runUntilIdle();

    const log = engine.log();
    expect(log.length).toBe(5);

    const types = log.map((e) => e.event.type);
    expect(types).toEqual([
      "TxAttempt",
      "TxStart",
      "FrontArrive",
      "TxEnd",
      "BackArrive",
    ]);

    // FrontArrive at s2 fires at τ = 5 µs = 5_000_000 ps.
    const frontArrive = log.find((e) => e.event.type === "FrontArrive");
    expect(frontArrive).toBeDefined();
    expect(frontArrive!.key.time).toBe(5_000_000n);
    if (frontArrive!.event.type === "FrontArrive") {
      expect(frontArrive!.event.node).toBe(s2);
    }

    // TxEnd at t = D_σ = 51.2 µs = 51_200_000 ps.
    const txEnd = log.find((e) => e.event.type === "TxEnd");
    expect(txEnd).toBeDefined();
    expect(txEnd!.key.time).toBe(51_200_000n);

    // BackArrive at t = τ + D_σ = 56.2 µs.
    const backArrive = log.find((e) => e.event.type === "BackArrive");
    expect(backArrive).toBeDefined();
    expect(backArrive!.key.time).toBe(56_200_000n);
  });
});

describe("typed observables", () => {
  it("carrierSense matches the half-open occupancy of the HD-1 signal", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
    const world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    const frame = engine.registerFrame(s1, 512n, false, 10_000_000n);
    engine.scheduleTxAttempt(0n, s1, frame);
    engine.runUntilIdle();
    const log = engine.log();

    // Before FrontArrive: no carrier.
    expect(carrierSense(log, s2, 0n)).toBe(false);
    // Mid-occupancy at receiver (5µs..56.2µs): carrier present.
    expect(carrierSense(log, s2, 30_000_000n)).toBe(true);
    // After BackArrive: no carrier.
    expect(carrierSense(log, s2, 60_000_000n)).toBe(false);
  });
});

describe("Event union exhaustiveness (compile-time check)", () => {
  it("every Event.type variant is covered by an exhaustive switch", () => {
    // This function exists purely to make tsc fail if an Event variant
    // is added in the Rust core without updating the TS union.
    function eventLabel(e: Event): string {
      switch (e.type) {
        case "TxAttempt":
          return "tx-attempt";
        case "TxStart":
          return "tx-start";
        case "TxEnd":
          return "tx-end";
        case "FrontArrive":
          return "front-arrive";
        case "BackArrive":
          return "back-arrive";
        case "CollisionDetect":
          return "collision-detect";
        case "JamStart":
          return "jam-start";
        case "JamEnd":
          return "jam-end";
        case "FrameEligible":
          return "frame-eligible";
        case "Enqueue":
          return "enqueue";
        case "Dequeue":
          return "dequeue";
        case "BackoffExpire":
          return "backoff-expire";
        case "SegmentAdded":
          return "segment-added";
        case "SegmentRemoved":
          return "segment-removed";
        case "NodeAdded":
          return "node-added";
        case "NodeRemoved":
          return "node-removed";
        case "SegmentDelayChanged":
          return "segment-delay-changed";
        case "SegmentRateChanged":
          return "segment-rate-changed";
        case "MacConfigChanged":
          return "mac-config-changed";
        case "PortDisconnected":
          return "port-disconnected";
        case "SignalLost":
          return "signal-lost";
        case "DeviceCommandApplied":
          return "device-command-applied";
        case "AgingTick":
          return "aging-tick";
        default: {
          const _exhaustive: never = e;
          return _exhaustive;
        }
      }
    }
    const sample: Event = { type: "TxAttempt", node: 0, frame: 0 };
    expect(eventLabel(sample)).toBe("tx-attempt");
  });
});
