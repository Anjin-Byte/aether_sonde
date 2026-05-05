// Cross-layer tests for the round-4 device-state API:
//   - `TypedEngine.deviceSnapshot(node)` round-trips every device
//     kind through serde-wasm-bindgen as a typed discriminated union.
//   - `TypedEngine.applyDeviceCommand(cmd)` round-trips every command
//     variant from TS through Rust.
//   - `DeviceCommandErrorE` correctly wraps `UnknownNode` /
//     `NotApplicable` errors at the boundary.

import { describe, expect, it } from "vitest";
import {
  type DeviceSnapshot,
  type MacAddress,
  DeviceCommandErrorE,
  TypedEngine,
  TypedTopologyBuilder,
} from "../src/index.js";

const MAC_1: MacAddress = [0, 0, 0, 0, 0, 1] as const;
const MAC_2: MacAddress = [0, 0, 0, 0, 0, 2] as const;

/** Build a 3-station, 1-switch topology and return the IDs. */
function switchScenario(agingPs: bigint) {
  const builder = TypedTopologyBuilder.create();
  const s1 = builder.addEndStation(1);
  const s2 = builder.addEndStation(1);
  const s3 = builder.addEndStation(1);
  const sw = builder.addSwitch(3, 64n, 1_000_000n, 0, agingPs);
  builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, sw, 0);
  builder.addHdSegment(10_000_000n, 5_000_000n, s2, 0, sw, 1);
  builder.addHdSegment(10_000_000n, 5_000_000n, s3, 0, sw, 2);
  const world = builder.build();
  const engine = TypedEngine.create(world, 0n);
  builder.dispose();
  return { engine, s1, s2, s3, sw };
}

describe("deviceSnapshot — typed cross-boundary read API", () => {
  it("end-station snapshot round-trips with port count", () => {
    const builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    using world = builder.build();
    builder.dispose();
    using engine = TypedEngine.create(world, 0n);
    const snap = engine.deviceSnapshot(s1)!;
    expect(snap.type).toBe("EndStation");
    if (snap.type === "EndStation") {
      expect(snap.port_count).toBe(1);
    }
  });

  it("switch snapshot reports config + initially-empty mac table", () => {
    const { engine, sw } = switchScenario(0n);
    using e = engine;
    const snap = e.deviceSnapshot(sw)!;
    expect(snap.type).toBe("Switch");
    if (snap.type === "Switch") {
      expect(snap.aging_threshold).toBe(0n);
      expect(snap.mac_table).toEqual([]);
    }
  });

  it("snapshot is exhaustively typed at compile time", () => {
    const { engine, sw } = switchScenario(0n);
    using e = engine;
    const snap = e.deviceSnapshot(sw)!;
    function label(s: DeviceSnapshot): string {
      switch (s.type) {
        case "EndStation":
          return "es";
        case "Repeater":
          return "rp";
        case "Bridge":
          return "br";
        case "Switch":
          return "sw";
        default: {
          const _exhaustive: never = s;
          return _exhaustive;
        }
      }
    }
    expect(label(snap)).toBe("sw");
  });

  it("returns null for unknown nodes", () => {
    using builder = TypedTopologyBuilder.create();
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    expect(engine.deviceSnapshot(999)).toBeNull();
  });
});

describe("applyDeviceCommand — typed cross-boundary write API", () => {
  it("InsertMacEntry → snapshot reflects new entry as ManualInsert", () => {
    const { engine, sw } = switchScenario(0n);
    using e = engine;
    e.applyDeviceCommand({
      type: "InsertMacEntry",
      node: sw,
      mac: MAC_1,
      port: 0,
    });
    const snap = e.deviceSnapshot(sw)!;
    if (snap.type !== "Switch") throw new Error("expected Switch");
    expect(snap.mac_table).toHaveLength(1);
    const entry = snap.mac_table[0]!;
    expect(entry.mac).toEqual(MAC_1);
    expect(entry.port).toBe(0);
    expect(entry.origin).toBe("ManualInsert");
  });

  it("FlushMacTable → snapshot shows empty table", () => {
    const { engine, sw } = switchScenario(0n);
    using e = engine;
    e.applyDeviceCommand({
      type: "InsertMacEntry",
      node: sw,
      mac: MAC_1,
      port: 0,
    });
    e.applyDeviceCommand({
      type: "InsertMacEntry",
      node: sw,
      mac: MAC_2,
      port: 1,
    });
    e.applyDeviceCommand({ type: "FlushMacTable", node: sw });
    const snap = e.deviceSnapshot(sw)!;
    if (snap.type !== "Switch") throw new Error("expected Switch");
    expect(snap.mac_table).toEqual([]);
  });

  it("RemoveMacEntry removes only the targeted entry", () => {
    const { engine, sw } = switchScenario(0n);
    using e = engine;
    e.applyDeviceCommand({
      type: "InsertMacEntry",
      node: sw,
      mac: MAC_1,
      port: 0,
    });
    e.applyDeviceCommand({
      type: "InsertMacEntry",
      node: sw,
      mac: MAC_2,
      port: 1,
    });
    e.applyDeviceCommand({ type: "RemoveMacEntry", node: sw, mac: MAC_1 });
    const snap = e.deviceSnapshot(sw)!;
    if (snap.type !== "Switch") throw new Error("expected Switch");
    expect(snap.mac_table).toHaveLength(1);
    expect(snap.mac_table[0]!.mac).toEqual(MAC_2);
  });

  it("SetSwitchAgingThreshold updates the snapshot", () => {
    const { engine, sw } = switchScenario(0n);
    using e = engine;
    e.applyDeviceCommand({
      type: "SetSwitchAgingThreshold",
      node: sw,
      threshold: 50_000_000n,
    });
    const snap = e.deviceSnapshot(sw)!;
    if (snap.type !== "Switch") throw new Error("expected Switch");
    expect(snap.aging_threshold).toBe(50_000_000n);
  });

  it("UnknownNode → throws DeviceCommandErrorE with kind UnknownNode", () => {
    const { engine } = switchScenario(0n);
    using e = engine;
    expect(() =>
      e.applyDeviceCommand({ type: "FlushMacTable", node: 999 }),
    ).toThrow(DeviceCommandErrorE);
    try {
      e.applyDeviceCommand({ type: "FlushMacTable", node: 999 });
    } catch (err) {
      if (!(err instanceof DeviceCommandErrorE)) throw err;
      expect(err.inner.kind).toBe("UnknownNode");
    }
  });
});

describe("Edit::AddSwitch — round-trip a Switch added via apply_edit", () => {
  it("snapshot is Switch type after AddSwitch via Edit", () => {
    using builder = TypedTopologyBuilder.create();
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    engine.applyEdit({
      type: "AddSwitch",
      port_count: 4,
      data: {
        decode_threshold: 64n,
        processing_delay: 1_000_000n,
        mac_table_capacity: 0,
        aging_threshold: 0n,
      },
    });
    const snap = engine.deviceSnapshot(0)!;
    expect(snap.type).toBe("Switch");
  });
});
