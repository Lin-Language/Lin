export class ScanResults {
  constructor(bestArrivals, kArrivals, kConnections) {
    this.k = 0;
    this.bestArrivals = bestArrivals;
    this.kArrivals = kArrivals;
    this.kConnections = kConnections;
  }

  addRound() {
    this.kArrivals[++this.k] = {};
  }

  previousArrival(stopPi) {
    return this.kArrivals[this.k - 1][stopPi];
  }

  setTrip(trip, startIndex, endIndex, interchange) {
    const time = trip.stopTimes[endIndex].arrivalTime + interchange;
    const stopPi = trip.stopTimes[endIndex].stop;

    this.kArrivals[this.k][stopPi] = time;
    this.bestArrivals[stopPi] = time;
    this.kConnections[stopPi][this.k] = [trip, startIndex, endIndex];
  }

  setTransfer(transfer, time) {
    const stopPi = transfer.destination;

    this.kArrivals[this.k][stopPi] = time;
    this.bestArrivals[stopPi] = time;
    this.kConnections[stopPi][this.k] = transfer;
  }

  bestArrival(stopPi) {
    return this.bestArrivals[stopPi];
  }

  getMarkedStops() {
    return Object.keys(this.kArrivals[this.k]);
  }

  finalize() {
    return [this.kConnections, this.bestArrivals];
  }
}
