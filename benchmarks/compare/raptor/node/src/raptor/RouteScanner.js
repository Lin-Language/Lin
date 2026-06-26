/**
 * Returns trips for specific routes using global flat typed arrays.
 * Maintains a per-route scan position to avoid re-scanning already-checked trips.
 */
export class RouteScanner {
  constructor(numRoutes, routeTrips, arrivals, departures, routes, date, dow) {
    this.routeTrips = routeTrips;
    this.arrivals = arrivals;
    this.departures = departures;
    this.routes = routes;
    this.date = date;
    this.dow = dow;
    // routeScanPosition[r] = last trip index checked for route r (-1 = not started)
    this.routeScanPosition = new Int32Array(numRoutes).fill(-1);
  }

  /**
   * Return the earliest trip (and its global base offset) for the given route,
   * starting at stopPos, that departs at or after `time`.
   *
   * Returns [trip, tripBase] or [null, -1].
   */
  getTrip(routeIdx, stopPos, time, stopTimesBase, numStops, numTrips) {
    // Initialize scan position to last trip in route
    let scanPos = this.routeScanPosition[routeIdx];
    if (scanPos === -1) scanPos = numTrips - 1;

    const departures = this.departures;
    const routeTrips = this.routeTrips[routeIdx];
    let lastFound = null;
    let lastFoundBase = -1;

    for (let i = scanPos; i >= 0; i--) {
      // Hot path: read departure from global flat array
      const dep = departures[stopTimesBase + i * numStops + stopPos];
      if (dep < time) break;

      const trip = routeTrips[i];
      if (trip.service.runsOn(this.date, this.dow)) {
        lastFound = trip;
        lastFoundBase = stopTimesBase + i * numStops;
      }

      if (lastFound === null || lastFound === trip) {
        this.routeScanPosition[routeIdx] = i;
      }
    }

    return [lastFound, lastFoundBase];
  }
}

/**
 * Create the RouteScanner from prebuilt global arrays
 */
export class RouteScannerFactory {
  constructor(numRoutes, routeTrips, arrivals, departures, routes) {
    this.numRoutes = numRoutes;
    this.routeTrips = routeTrips;
    this.arrivals = arrivals;
    this.departures = departures;
    this.routes = routes;
  }

  create(date, dow) {
    return new RouteScanner(this.numRoutes, this.routeTrips, this.arrivals, this.departures, this.routes, date, dow);
  }
}
