/**
 * Create a queue for the Raptor algorithm to use on each iteration of the algorithm.
 * All identifiers are integer indices (no string hashing in the hot path).
 */
export class QueueFactory {
  constructor(numStops, stopRoutes, stopRoutesBase, stopRoutesCount, routeStopPos) {
    this.stopRoutes = stopRoutes;
    this.stopRoutesBase = stopRoutesBase;
    this.stopRoutesCount = stopRoutesCount;
    this.routeStopPos = routeStopPos;
  }

  /**
   * Take the marked stops (integer indices) and return a Map<routeIdx, startPos>
   * where startPos is the earliest position in the route to start scanning from.
   */
  getQueue(markedStops) {
    const queue = new Map();
    const stopRoutes = this.stopRoutes;
    const stopRoutesBase = this.stopRoutesBase;
    const stopRoutesCount = this.stopRoutesCount;
    const routeStopPos = this.routeStopPos;

    for (const stopIdx of markedStops) {
      const base = stopRoutesBase[stopIdx];
      const count = stopRoutesCount[stopIdx];
      for (let i = 0; i < count; i++) {
        const routeIdx = stopRoutes[base + i];
        const pos = routeStopPos[routeIdx].get(stopIdx);
        if (pos === undefined) continue;
        const existing = queue.get(routeIdx);
        if (existing === undefined || pos < existing) {
          queue.set(routeIdx, pos);
        }
      }
    }

    return queue;
  }
}
