import { ScanResults } from "./ScanResults.js";

const INF = 4_000_000_000;  // sentinel: large enough to dominate any arrival time, with headroom for +duration

export class ScanResultsFactory {
  constructor(numStops, stopIndexOf, stopNames) {
    this.numStops = numStops;
    this.stopIndexOf = stopIndexOf;
    this.stopNames = stopNames;
  }

  create(origins) {
    const numStops = this.numStops;

    // bestArrivals: dense typed array, indexed by int stop id
    const bestArrivals = new Float64Array(numStops).fill(INF);
    // kArrivals[0] = initial state (origins), same shape
    const k0 = new Float64Array(numStops).fill(INF);

    for (const [stopId, time] of Object.entries(origins)) {
      const idx = this.stopIndexOf.get(stopId);
      if (idx !== undefined) {
        bestArrivals[idx] = time;
        k0[idx] = time;
      }
    }

    // kConnections: per-stop, per-round connection record (for journey reconstruction)
    // Keyed by string stop id for compatibility with JourneyFactory.
    const stopNames = this.stopNames;
    const kConnections = {};
    for (let i = 0; i < numStops; i++) {
      kConnections[stopNames[i]] = Object.create(null);
    }

    return new ScanResults(numStops, bestArrivals, [k0], kConnections, this.stopIndexOf, stopNames);
  }
}
