/**
 * Implementation of the Raptor journey planning algorithm.
 * Uses integer-indexed global typed arrays throughout the hot path.
 */
export class RaptorAlgorithm {
  constructor(
    numStops,
    numRoutes,
    stopIndexOf,
    stopNames,
    routes,
    routeStops,
    arrivals,
    departures,
    routeTrips,
    routeStopPos,
    stopRoutes,
    stopRoutesBase,
    stopRoutesCount,
    interchange,
    transfers,
    scanResultsFactory,
    queueFactory,
    routeScannerFactory,
  ) {
    this.numStops = numStops;
    this.numRoutes = numRoutes;
    this.stopIndexOf = stopIndexOf;
    this.stopNames = stopNames;
    this.routes = routes;
    this.routeStops = routeStops;
    this.arrivals = arrivals;
    this.departures = departures;
    this.routeTrips = routeTrips;
    this.routeStopPos = routeStopPos;
    this.stopRoutes = stopRoutes;
    this.stopRoutesBase = stopRoutesBase;
    this.stopRoutesCount = stopRoutesCount;
    this.interchange = interchange;
    this.transfers = transfers;
    this.scanResultsFactory = scanResultsFactory;
    this.queueFactory = queueFactory;
    this.routeScannerFactory = routeScannerFactory;
  }

  /**
   * Perform a plan of the routes at a given time and return the resulting kConnections index.
   * Origins is a map: string stopId -> departure time.
   */
  scan(origins, date, dow) {
    const routeScanner = this.routeScannerFactory.create(date, dow);
    const results = this.scanResultsFactory.create(origins);

    // Convert string origins to integer marked stops list
    let markedStops = [];
    for (const stopId of Object.keys(origins)) {
      const idx = this.stopIndexOf.get(stopId);
      if (idx !== undefined) markedStops.push(idx);
    }

    while (markedStops.length > 0) {
      results.addRound();

      this.scanRoutes(results, routeScanner, markedStops);
      this.scanTransfers(results, markedStops);

      markedStops = results.getMarkedStops();
    }

    return results.finalize();
  }

  scanRoutes(results, routeScanner, markedStops) {
    const queue = this.queueFactory.getQueue(markedStops);
    // queue is a Map<routeIdx (int), stopPos (int in route)>

    const routeStops = this.routeStops;
    const interchange = this.interchange;
    const arrivals = this.arrivals;

    for (const [routeIdx, startPos] of queue) {
      let boardingPoint = -1;
      let trip = null;
      let tripBase = -1;  // index into global arrivals/departures for current trip

      const { stopTimesBase, numStops, numTrips, routeStopsBase } = this.routes[routeIdx];

      for (let pi = startPos; pi < numStops; pi++) {
        const stopIdx = routeStops[routeStopsBase + pi];
        const previousArrival = results.previousArrival(stopIdx);

        if (trip !== null) {
          const iVal = interchange[stopIdx];
          const arrTime = arrivals[tripBase + pi] + iVal;

          if (trip.stopTimes[pi].dropOff && arrTime < results.bestArrival(stopIdx)) {
            results.setTrip(trip, boardingPoint, pi, iVal, stopIdx, arrivals[tripBase + pi]);
          } else if (previousArrival !== -1 && previousArrival < arrivals[tripBase + pi] + iVal) {
            const [newTrip, newBase] = routeScanner.getTrip(routeIdx, pi, previousArrival, stopTimesBase, numStops, numTrips);
            if (newTrip !== null) {
              trip = newTrip;
              tripBase = newBase;
              boardingPoint = pi;
            }
          }
        } else if (previousArrival !== -1) {
          const [newTrip, newBase] = routeScanner.getTrip(routeIdx, pi, previousArrival, stopTimesBase, numStops, numTrips);
          if (newTrip !== null) {
            trip = newTrip;
            tripBase = newBase;
            boardingPoint = pi;
          }
        }
      }
    }
  }

  scanTransfers(results, markedStops) {
    const transfers = this.transfers;
    const interchange = this.interchange;

    for (const stopIdx of markedStops) {
      const xfers = transfers[stopIdx];
      if (xfers.length === 0) continue;
      const prevArr = results.previousArrival(stopIdx);
      if (prevArr === -1) continue;

      for (const transfer of xfers) {
        const destIdx = transfer.destIdx;
        const arrival = prevArr + transfer.duration + interchange[destIdx];

        if (transfer.startTime <= arrival && transfer.endTime >= arrival && arrival < results.bestArrival(destIdx)) {
          results.setTransfer(transfer, arrival, destIdx);
        }
      }
    }
  }
}
