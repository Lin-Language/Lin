import { ScanResults } from "./ScanResults.js";

export class ScanResultsFactory {
  constructor(stops) {
    this.stops = stops;
  }

  create(origins) {
    const bestArrivals = Object.fromEntries(
      this.stops.map(stop => [stop, origins[stop] || Number.MAX_SAFE_INTEGER])
    );
    const kArrivals = [Object.fromEntries(
      this.stops.map(stop => [stop, origins[stop] || Number.MAX_SAFE_INTEGER])
    )];
    const kConnections = Object.fromEntries(
      this.stops.map(stop => [stop, Object.create(null)])
    );

    return new ScanResults(bestArrivals, kArrivals, kConnections);
  }
}
