// Exhaustive `Edit` variant round-trips through the typed adapter.
// Every variant of the `Edit` discriminated union is constructed in TS,
// applied via `engine.applyEdit`, and verified to produce the
// corresponding topology event in the typed log.

import { describe, expect, it } from "vitest";
import {
  type Edit,
  type Event,
  MAC_CONFIG_IEEE_802_3,
  TypedEngine,
  TypedTopologyBuilder,
} from "../src/index.js";

/** Build a 2-station HD pair with a single segment. Returns engine + node IDs. */
function hdPair(): {
  engine: TypedEngine;
  s1: number;
  s2: number;
  segment: number;
} {
  using builder = TypedTopologyBuilder.create();
  const s1 = builder.addEndStation(2);
  const s2 = builder.addEndStation(2);
  const segment = builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
  const world = builder.build();
  const engine = TypedEngine.create(world, 0n);
  return { engine, s1, s2, segment };
}

/** Apply an edit, drain the queue, and return the produced log. */
function applyAndDrain(engine: TypedEngine, edit: Edit): readonly Event[] {
  engine.applyEdit(edit);
  engine.runUntilIdle();
  return engine.log().map((e) => e.event);
}

// ---------------------------------------------------------------------------
// Node-add variants → NodeAdded
// ---------------------------------------------------------------------------

describe("AddEndStation", () => {
  it("produces NodeAdded with the expected fresh node id", () => {
    using builder = TypedTopologyBuilder.create();
    const world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    const events = applyAndDrain(engine, { type: "AddEndStation", port_count: 1 });
    expect(events.some((e) => e.type === "NodeAdded" && e.node === 0)).toBe(true);
    expect(engine.nodeCount()).toBe(1);
  });
});

describe("AddRepeater", () => {
  it("produces NodeAdded for the new repeater", () => {
    using builder = TypedTopologyBuilder.create();
    const world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    const events = applyAndDrain(engine, {
      type: "AddRepeater",
      port_count: 2,
      delta_h: 100n,
    });
    expect(events.some((e) => e.type === "NodeAdded")).toBe(true);
    expect(engine.nodeCount()).toBe(1);
  });
});

describe("AddBridge", () => {
  it("produces NodeAdded for the new bridge", () => {
    using builder = TypedTopologyBuilder.create();
    const world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    const events = applyAndDrain(engine, {
      type: "AddBridge",
      port_count: 2,
      decode_threshold: 64n,
      processing_delay: 500n,
    });
    expect(events.some((e) => e.type === "NodeAdded")).toBe(true);
    expect(engine.nodeCount()).toBe(1);
  });
});

// ---------------------------------------------------------------------------
// Segment-add variants → SegmentAdded
// ---------------------------------------------------------------------------

describe("AddHdSegment", () => {
  it("produces SegmentAdded with kind Hd", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    const world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    const events = applyAndDrain(engine, {
      type: "AddHdSegment",
      rate: 10_000_000n,
      delay: 1_000_000n,
      a: { node: s1, port: 0 },
      b: { node: s2, port: 0 },
    });
    expect(
      events.some((e) => e.type === "SegmentAdded" && e.kind === "Hd"),
    ).toBe(true);
  });
});

describe("AddFdSegment", () => {
  it("produces SegmentAdded with kind Fd", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    const world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    const events = applyAndDrain(engine, {
      type: "AddFdSegment",
      rate: 1_000_000_000n,
      delay: 100n,
      a: { node: s1, port: 0 },
      b: { node: s2, port: 0 },
    });
    expect(
      events.some((e) => e.type === "SegmentAdded" && e.kind === "Fd"),
    ).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Removal variants → SegmentRemoved / NodeRemoved / PortDisconnected
// ---------------------------------------------------------------------------

describe("RemoveSegment", () => {
  it("produces SegmentRemoved with the matching segment id", () => {
    const { engine, segment } = hdPair();
    const events = applyAndDrain(engine, { type: "RemoveSegment", segment });
    expect(
      events.some(
        (e) => e.type === "SegmentRemoved" && e.segment === segment,
      ),
    ).toBe(true);
  });
});

describe("RemoveNode", () => {
  it("produces NodeRemoved with the matching node id", () => {
    const { engine, s1 } = hdPair();
    const events = applyAndDrain(engine, { type: "RemoveNode", node: s1 });
    expect(events.some((e) => e.type === "NodeRemoved" && e.node === s1)).toBe(
      true,
    );
  });
});

describe("DisconnectPort", () => {
  it("produces PortDisconnected with the matching node + port", () => {
    const { engine, s1 } = hdPair();
    const events = applyAndDrain(engine, {
      type: "DisconnectPort",
      node: s1,
      port: 0,
    });
    expect(
      events.some(
        (e) => e.type === "PortDisconnected" && e.node === s1 && e.port === 0,
      ),
    ).toBe(true);
  });
});

// ---------------------------------------------------------------------------
// Parameter-change variants → SegmentDelayChanged / SegmentRateChanged
// ---------------------------------------------------------------------------

describe("SetSegmentDelay", () => {
  it("produces SegmentDelayChanged with old and new fields", () => {
    const { engine, segment } = hdPair();
    const events = applyAndDrain(engine, {
      type: "SetSegmentDelay",
      segment,
      new_delay: 2_000_000n,
    });
    const change = events.find((e) => e.type === "SegmentDelayChanged");
    expect(change).toBeDefined();
    if (change?.type === "SegmentDelayChanged") {
      expect(change.old).toBe(5_000_000n);
      expect(change.new).toBe(2_000_000n);
    }
  });
});

describe("SetSegmentRate", () => {
  it("produces SegmentRateChanged with old and new fields", () => {
    const { engine, segment } = hdPair();
    const events = applyAndDrain(engine, {
      type: "SetSegmentRate",
      segment,
      new_rate: 100_000_000n,
    });
    const change = events.find((e) => e.type === "SegmentRateChanged");
    expect(change).toBeDefined();
    if (change?.type === "SegmentRateChanged") {
      expect(change.old).toBe(10_000_000n);
      expect(change.new).toBe(100_000_000n);
    }
  });
});

// ---------------------------------------------------------------------------
// MAC config variant → MacConfigChanged
// ---------------------------------------------------------------------------

describe("SetMacConfig", () => {
  it("produces MacConfigChanged for the targeted node", () => {
    const { engine, s1 } = hdPair();
    const events = applyAndDrain(engine, {
      type: "SetMacConfig",
      node: s1,
      config: MAC_CONFIG_IEEE_802_3,
    });
    expect(
      events.some((e) => e.type === "MacConfigChanged" && e.node === s1),
    ).toBe(true);
  });
});
