// Typed wrapper around the raw `Engine`. Adds: typed `Edit`/`Event`
// payloads, typed log retrieval, and error mapping.

import { Engine as RawEngine } from "aether-sonde-wasm";
import { TypedWorld } from "./builder.js";
import {
  type DeviceCommand,
  type DeviceCommandError,
  DeviceCommandErrorE,
  type DeviceSnapshot,
  type Edit,
  type EditError,
  EditErrorE,
  type EngineError,
  EngineErrorE,
  type LoggedEvent,
  type LogSnapshot,
  type MacConfig,
} from "./types.js";

function asEditError(e: unknown): EditErrorE {
  if (typeof e === "object" && e !== null && "kind" in e) {
    return new EditErrorE(e as EditError);
  }
  return new EditErrorE({ kind: "InvalidEdit", reason: String(e) });
}

function asEngineError(e: unknown): EngineErrorE {
  if (typeof e === "object" && e !== null && "kind" in e) {
    return new EngineErrorE(e as EngineError);
  }
  return new EngineErrorE({ kind: "ZeroBitFrame" });
}

function asDeviceCommandError(e: unknown): DeviceCommandErrorE {
  if (typeof e === "object" && e !== null && "kind" in e) {
    return new DeviceCommandErrorE(e as DeviceCommandError);
  }
  return new DeviceCommandErrorE({
    kind: "InvalidArgument",
    reason: String(e),
  });
}

/**
 * The discrete-event scheduler. Owns the `TypedWorld` it was
 * constructed with (the world is consumed at construction).
 *
 * Throws `EditErrorE` from `applyEdit`, `EngineErrorE` from
 * `registerFrame`. All other methods return normally or are infallible.
 */
export class TypedEngine implements Disposable {
  private constructor(private inner: RawEngine) {}

  /** Build an engine wrapping `world`. The seed drives BEB. */
  static create(world: TypedWorld, seed: bigint): TypedEngine {
    return new TypedEngine(new RawEngine(world.unwrap(), seed));
  }

  nodeCount(): number {
    return this.inner.nodeCount();
  }

  /** Set MAC config for `node`. */
  setMacConfig(node: number, config: MacConfig): void {
    this.inner.setMacConfig(node, config);
  }

  /**
   * Register a frame for transmission. Returns the assigned `FrameId`.
   * Throws `EngineErrorE` if `bits == 0n`.
   */
  registerFrame(
    sourceNode: number,
    bits: bigint,
    isJam: boolean,
    rateBps: bigint,
  ): number {
    try {
      return this.inner.registerFrame(sourceNode, bits, isJam, rateBps);
    } catch (e: unknown) {
      throw asEngineError(e);
    }
  }

  /** Schedule a `TxAttempt` at the given simulation time. */
  scheduleTxAttempt(timePs: bigint, node: number, frame: number): void {
    this.inner.scheduleTxAttempt(timePs, node, frame);
  }

  /** Run the dispatch loop until `timePs` (picoseconds). */
  runUntil(timePs: bigint): void {
    this.inner.runUntil(timePs);
  }

  /** Drain the queue. */
  runUntilIdle(): void {
    this.inner.runUntilIdle();
  }

  /**
   * Apply a topology `Edit`. Throws `EditErrorE` on validation
   * failure; consumers narrow on `.inner.kind`.
   */
  applyEdit(edit: Edit): void {
    try {
      this.inner.applyEdit(edit);
    } catch (e: unknown) {
      throw asEditError(e);
    }
  }

  /**
   * Snapshot the engine's event log. Returns the array of typed
   * `LoggedEvent` entries directly — drops the `LogSnapshot` wrapper
   * for ergonomic indexing.
   */
  log(): readonly LoggedEvent[] {
    const raw = this.inner.log() as LogSnapshot;
    return raw.entries;
  }

  /**
   * Snapshot the full log as a JSON-friendly object. Useful for
   * determinism comparisons via `JSON.stringify`.
   */
  logSnapshot(): LogSnapshot {
    return this.inner.log() as LogSnapshot;
  }

  /**
   * Typed snapshot of `node`'s link-layer device state, or `null`
   * if `node` is unknown. The returned object is a discriminated
   * union tagged by `type` — pattern-match to access per-device
   * fields.
   */
  deviceSnapshot(node: number): DeviceSnapshot | null {
    const raw = this.inner.deviceSnapshot(node);
    return raw === null || raw === undefined ? null : (raw as DeviceSnapshot);
  }

  /**
   * Apply a typed `DeviceCommand` mid-simulation. Throws
   * `DeviceCommandErrorE` on validation failure; consumers narrow
   * on `.inner.kind`.
   */
  applyDeviceCommand(cmd: DeviceCommand): void {
    try {
      this.inner.applyDeviceCommand(cmd);
    } catch (e: unknown) {
      throw asDeviceCommandError(e);
    }
  }

  dispose(): void {
    this.inner.free();
  }

  [Symbol.dispose](): void {
    this.dispose();
  }
}
