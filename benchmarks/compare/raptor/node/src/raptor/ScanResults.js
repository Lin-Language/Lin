const INF = 4_000_000_000;

export class ScanResults {
  constructor(numStops, bestArrivals, kArrivals, kConnections, stopIndexOf, stopNames) {
    this.k = 0;
    this.numStops = numStops;
    this.bestArrivals = bestArrivals;   // Float64Array[numStops]
    this.kArrivals = kArrivals;         // Array of Float64Array[numStops]
    this.kConnections = kConnections;   // {stopName: {round: connection}} for reconstruction
    this.stopIndexOf = stopIndexOf;
    this.stopNames = stopNames;
    // Marked stops this round (int indices)
    this._markedThisRound = [];
  }

  addRound() {
    this.k++;
    this.kArrivals.push(new Float64Array(this.numStops).fill(INF));
    this._markedThisRound = [];
  }

  previousArrival(stopIdx) {
    const v = this.kArrivals[this.k - 1][stopIdx];
    return v >= INF ? -1 : v;
  }

  setTrip(trip, startIndex, endIndex, interchangeVal, stopIdx, rawArrival) {
    const time = rawArrival + interchangeVal;
    this.kArrivals[this.k][stopIdx] = time;
    this.bestArrivals[stopIdx] = time;
    this._markedThisRound.push(stopIdx);

    const stopName = this.stopNames[stopIdx];
    this.kConnections[stopName][this.k] = [trip, startIndex, endIndex];
  }

  setTransfer(transfer, time, destIdx) {
    this.kArrivals[this.k][destIdx] = time;
    this.bestArrivals[destIdx] = time;
    this._markedThisRound.push(destIdx);

    const stopName = this.stopNames[destIdx];
    this.kConnections[stopName][this.k] = transfer;
  }

  bestArrival(stopIdx) {
    const v = this.bestArrivals[stopIdx];
    return v >= INF ? INF : v;
  }

  getMarkedStops() {
    // Return the list of int stop indices improved this round, deduplicated.
    // (A stop can be marked multiple times in one round via different routes/transfers.)
    if (this._markedThisRound.length <= 1) return this._markedThisRound.slice();
    return [...new Set(this._markedThisRound)];
  }

  finalize() {
    // Return kConnections keyed by string stop ids (already is), plus bestArrivals
    // converted back to a string-keyed map for GroupStationDepartAfterQuery.getFoundStations.
    const bestArrivalsMap = {};
    const stopNames = this.stopNames;
    const ba = this.bestArrivals;
    for (let i = 0; i < this.numStops; i++) {
      if (ba[i] < INF) bestArrivalsMap[stopNames[i]] = ba[i];
    }
    return [this.kConnections, bestArrivalsMap];
  }
}
