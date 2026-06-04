/**
 * Implementation of the Raptor journey planning algorithm
 */
export class RaptorAlgorithm {
  constructor(routeStopIndex, routePath, transfers, interchange, scanResultsFactory, queueFactory, routeScannerFactory) {
    this.routeStopIndex = routeStopIndex;
    this.routePath = routePath;
    this.transfers = transfers;
    this.interchange = interchange;
    this.scanResultsFactory = scanResultsFactory;
    this.queueFactory = queueFactory;
    this.routeScannerFactory = routeScannerFactory;
  }

  /**
   * Perform a plan of the routes at a given time and return the resulting kConnections index
   */
  scan(origins, date, dow) {
    const routeScanner = this.routeScannerFactory.create(date, dow);
    const results = this.scanResultsFactory.create(origins);
    let markedStops = Object.keys(origins);

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

    for (const [routeId, stopP] of Object.entries(queue)) {
      let boardingPoint = -1;
      let trip;
      const routePath = this.routePath[routeId];
      const routePathLength = routePath.length;

      for (let pi = this.routeStopIndex[routeId][stopP]; pi < routePathLength; pi++) {
        const stopPi = routePath[pi];
        const previousArrival = results.previousArrival(stopPi);

        if (trip) {
          const i = this.interchange[stopPi];
          const stopTime = trip.stopTimes[pi];

          if (stopTime.dropOff && stopTime.arrivalTime + i < results.bestArrival(stopPi)) {
            results.setTrip(trip, boardingPoint, pi, i);
          }
          else if (previousArrival && previousArrival < stopTime.arrivalTime + i) {
            const newTrip = routeScanner.getTrip(routeId, pi, previousArrival);

            if (newTrip) {
              trip = newTrip;
              boardingPoint = pi;
            }
          }
        }
        else if (previousArrival) {
          const newTrip = routeScanner.getTrip(routeId, pi, previousArrival);

          if (newTrip) {
            trip = newTrip;
            boardingPoint = pi;
          }
        }
      }
    }
  }

  scanTransfers(results, markedStops) {
    for (const stopP of markedStops) {
      for (const transfer of this.transfers[stopP] || []) {
        const stopPi = transfer.destination;
        const arrival = results.previousArrival(stopP) + transfer.duration + this.interchange[stopPi];

        if (transfer.startTime <= arrival && transfer.endTime >= arrival && arrival < results.bestArrival(stopPi)) {
          results.setTransfer(transfer, arrival);
        }
      }
    }
  }
}
