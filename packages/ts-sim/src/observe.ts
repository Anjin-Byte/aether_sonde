// Typed wrappers around the four observable functions. The raw
// observables take a `LogSnapshot`-shaped object; the typed wrappers
// repackage it transparently so consumers can pass either an
// `LoggedEvent[]` (from `engine.log()`) or a full `LogSnapshot`.

import {
  carrierSense as rawCarrierSense,
  collisionDetect as rawCollisionDetect,
  firstCollisionDetectAt as rawFirstCollisionDetectAt,
} from "aether-sonde-wasm";
import { type LoggedEvent, type LogSnapshot } from "./types.js";

function asSnapshot(
  log: LogSnapshot | readonly LoggedEvent[],
): LogSnapshot {
  if (Array.isArray(log)) {
    return { entries: log };
  }
  return log as LogSnapshot;
}

/** True iff at least one signal is occupying `node` at time `tPs`. */
export function carrierSense(
  log: LogSnapshot | readonly LoggedEvent[],
  node: number,
  tPs: bigint,
): boolean {
  return rawCarrierSense(asSnapshot(log), node, tPs);
}

/** True iff `node` has experienced a collision by time `tPs`. */
export function collisionDetect(
  log: LogSnapshot | readonly LoggedEvent[],
  node: number,
  tPs: bigint,
): boolean {
  return rawCollisionDetect(asSnapshot(log), node, tPs);
}

/** Earliest time `node` observed a collision, or `null` if none. */
export function firstCollisionDetectAt(
  log: LogSnapshot | readonly LoggedEvent[],
  node: number,
): bigint | null {
  const result = rawFirstCollisionDetectAt(asSnapshot(log), node);
  return result === undefined ? null : result;
}
