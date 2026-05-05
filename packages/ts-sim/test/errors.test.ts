// Error round-trip: every error subclass narrows on `.inner.kind`,
// and `instanceof Error` semantics are preserved.

import { describe, expect, it } from "vitest";
import {
  BuildErrorE,
  EditErrorE,
  TypedEngine,
  TypedTopologyBuilder,
} from "../src/index.js";

describe("BuildErrorE", () => {
  it("is thrown when an HD segment has zero delay", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    expect(() => builder.addHdSegment(10_000_000n, 0n, s1, 0, s2, 0)).toThrow(
      BuildErrorE,
    );
    try {
      builder.addHdSegment(10_000_000n, 0n, s1, 0, s2, 0);
    } catch (e) {
      expect(e).toBeInstanceOf(BuildErrorE);
      expect(e).toBeInstanceOf(Error);
      const err = e as BuildErrorE;
      expect(err.inner.kind).toBe("ZeroDelay");
    }
  });

  it("is thrown for endpoints on the same node", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(2);
    try {
      builder.addHdSegment(10_000_000n, 1_000n, s1, 0, s1, 1);
      throw new Error("expected throw");
    } catch (e) {
      expect(e).toBeInstanceOf(BuildErrorE);
      const err = e as BuildErrorE;
      expect(err.inner.kind).toBe("EndpointsOnSameNode");
    }
  });
});

describe("EditErrorE", () => {
  it("UnknownNode: applyEdit on a non-existent node", () => {
    using builder = TypedTopologyBuilder.create();
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    try {
      engine.applyEdit({
        type: "RemoveNode",
        node: 99,
      });
      throw new Error("expected throw");
    } catch (e) {
      expect(e).toBeInstanceOf(EditErrorE);
      const err = e as EditErrorE;
      expect(err.inner.kind).toBe("UnknownNode");
      if (err.inner.kind === "UnknownNode") {
        expect(err.inner.node).toBe(99);
      }
    }
  });

  it("WouldViolateA7: closing a triangle in HD", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(2);
    const s2 = builder.addEndStation(2);
    const s3 = builder.addEndStation(2);
    builder.addHdSegment(10_000_000n, 1_000n, s1, 0, s2, 0);
    builder.addHdSegment(10_000_000n, 1_000n, s2, 1, s3, 0);
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    try {
      engine.applyEdit({
        type: "AddHdSegment",
        rate: 10_000_000n,
        delay: 1_000n,
        a: { node: s3, port: 1 },
        b: { node: s1, port: 1 },
      });
      throw new Error("expected throw");
    } catch (e) {
      expect(e).toBeInstanceOf(EditErrorE);
      const err = e as EditErrorE;
      expect(err.inner.kind).toBe("WouldViolateA7");
    }
  });

  it("InvalidEdit: zero delay on SetSegmentDelay", () => {
    using builder = TypedTopologyBuilder.create();
    const s1 = builder.addEndStation(1);
    const s2 = builder.addEndStation(1);
    builder.addHdSegment(10_000_000n, 5_000_000n, s1, 0, s2, 0);
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    try {
      engine.applyEdit({
        type: "SetSegmentDelay",
        segment: 0,
        new_delay: 0n,
      });
      throw new Error("expected throw");
    } catch (e) {
      expect(e).toBeInstanceOf(EditErrorE);
      const err = e as EditErrorE;
      expect(err.inner.kind).toBe("InvalidEdit");
    }
  });

  it("UnknownSegment: removing a non-existent segment", () => {
    using builder = TypedTopologyBuilder.create();
    using world = builder.build();
    using engine = TypedEngine.create(world, 0n);
    try {
      engine.applyEdit({
        type: "RemoveSegment",
        segment: 7,
      });
      throw new Error("expected throw");
    } catch (e) {
      expect(e).toBeInstanceOf(EditErrorE);
      const err = e as EditErrorE;
      expect(err.inner.kind).toBe("UnknownSegment");
    }
  });
});
