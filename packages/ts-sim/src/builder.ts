// Typed wrapper around the raw `TopologyBuilder` and `World` from
// `aether-sonde-wasm`. Adds error mapping (raw thrown objects → typed
// `BuildErrorE` instances) and `Symbol.dispose` plumbing.

import {
  TopologyBuilder as RawTopologyBuilder,
  World as RawWorld,
} from "aether-sonde-wasm";
import { type BuildError, BuildErrorE } from "./types.js";

function asBuildError(e: unknown): BuildErrorE {
  if (typeof e === "object" && e !== null && "kind" in e) {
    return new BuildErrorE(e as BuildError);
  }
  return new BuildErrorE({
    kind: "InvalidConfig",
    reason: String(e),
  });
}

/**
 * Mutable builder for a [`TypedWorld`]. Mirrors the Rust
 * `TopologyBuilder` 1:1, with `BuildError` thrown as `BuildErrorE`.
 *
 * `build()` consumes the underlying handle. After a successful `build()`,
 * subsequent calls (including `dispose()` / `[Symbol.dispose]()`) are
 * no-ops.
 */
export class TypedTopologyBuilder implements Disposable {
  private consumed = false;
  private constructor(private inner: RawTopologyBuilder) {}

  static create(): TypedTopologyBuilder {
    return new TypedTopologyBuilder(new RawTopologyBuilder());
  }

  /** Append an end-station. Returns the new node's numeric ID. */
  addEndStation(portCount: number): number {
    return this.inner.addEndStation(portCount);
  }

  /** Append a repeater. `deltaH` in picoseconds. */
  addRepeater(portCount: number, deltaH: bigint): number {
    return this.inner.addRepeater(portCount, deltaH);
  }

  /** Append a bridge. `decodeThreshold` in bits, `processingDelay` in picoseconds. */
  addBridge(
    portCount: number,
    decodeThreshold: bigint,
    processingDelay: bigint,
  ): number {
    return this.inner.addBridge(portCount, decodeThreshold, processingDelay);
  }

  /**
   * Append a learning switch.
   *
   * `decodeThreshold` in bits; `processingDelay` and `agingThresholdPs`
   * in picoseconds. `macTableCapacity == 0` means unbounded;
   * `agingThresholdPs == 0n` disables aging.
   */
  addSwitch(
    portCount: number,
    decodeThreshold: bigint,
    processingDelay: bigint,
    macTableCapacity: number,
    agingThresholdPs: bigint,
  ): number {
    return this.inner.addSwitch(
      portCount,
      decodeThreshold,
      processingDelay,
      macTableCapacity,
      agingThresholdPs,
    );
  }

  /** Append an HD segment. Throws `BuildErrorE` on failure. */
  addHdSegment(
    rateBps: bigint,
    delayPs: bigint,
    aNode: number,
    aPort: number,
    bNode: number,
    bPort: number,
  ): number {
    try {
      return this.inner.addHdSegment(
        rateBps,
        delayPs,
        aNode,
        aPort,
        bNode,
        bPort,
      );
    } catch (e: unknown) {
      throw asBuildError(e);
    }
  }

  /** Append an FD segment. Throws `BuildErrorE` on failure. */
  addFdSegment(
    rateBps: bigint,
    delayPs: bigint,
    aNode: number,
    aPort: number,
    bNode: number,
    bPort: number,
  ): number {
    try {
      return this.inner.addFdSegment(
        rateBps,
        delayPs,
        aNode,
        aPort,
        bNode,
        bPort,
      );
    } catch (e: unknown) {
      throw asBuildError(e);
    }
  }

  /**
   * Validate and finalize. Throws `BuildErrorE` on validation failure.
   * Consumes the builder; subsequent calls fail.
   */
  build(): TypedWorld {
    try {
      const world = new TypedWorld(this.inner.build());
      this.consumed = true;
      return world;
    } catch (e: unknown) {
      // build() consumes self even on Err; mark as consumed so dispose() is a no-op.
      this.consumed = true;
      throw asBuildError(e);
    }
  }

  /** Release the underlying WASM allocation. No-op if already consumed by `build()`. */
  dispose(): void {
    if (this.consumed) return;
    this.inner.free();
    this.consumed = true;
  }

  [Symbol.dispose](): void {
    this.dispose();
  }

  /** @internal */
  unwrap(): RawTopologyBuilder {
    return this.inner;
  }
}

/**
 * A validated topology. Returned by [`TypedTopologyBuilder.build`].
 * Topology mutation goes through `TypedEngine.applyEdit`.
 *
 * Consumed by `TypedEngine.create(world, seed)`. After consumption,
 * `dispose()` / `[Symbol.dispose]()` are no-ops.
 */
export class TypedWorld implements Disposable {
  private consumed = false;
  /** @internal */
  constructor(private inner: RawWorld) {}

  nodeCount(): number {
    return this.inner.nodeCount();
  }

  segmentCount(): number {
    return this.inner.segmentCount();
  }

  collisionResourceCount(): number {
    return this.inner.collisionResourceCount();
  }

  serializerCount(): number {
    return this.inner.serializerCount();
  }

  dispose(): void {
    if (this.consumed) return;
    this.inner.free();
    this.consumed = true;
  }

  [Symbol.dispose](): void {
    this.dispose();
  }

  /** @internal — consumed by `TypedEngine.create`. Marks the world as moved. */
  unwrap(): RawWorld {
    const inner = this.inner;
    this.consumed = true;
    return inner;
  }
}
