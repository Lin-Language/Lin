/**
 * Returns trips for specific routes. Maintains a reference to the last trip
 * returned in order to reduce plan time.
 */
export class RouteScanner {
  constructor(tripsByRoute, date, dow) {
    this.tripsByRoute = tripsByRoute;
    this.date = date;
    this.dow = dow;
    this.routeScanPosition = {};
  }

  /**
   * Return the earliest trip stop times possible on the given route
   */
  getTrip(routeId, stopIndex, time) {
    if (!Object.hasOwn(this.routeScanPosition, routeId)) {
      this.routeScanPosition[routeId] = this.tripsByRoute[routeId].length - 1;
    }

    let lastFound;
    const routeTrips = this.tripsByRoute[routeId];

    // iterate backwards through the trips on the route, starting where we last found a trip
    for (let i = this.routeScanPosition[routeId]; i >= 0; i--) {
      const trip = routeTrips[i];
      const stopTime = trip.stopTimes[stopIndex];

      // if the trip is unreachable, exit the loop
      if (stopTime.departureTime < time) {
        break;
      }
      // if it is reachable and the service is running that day, update the last valid trip found
      else if (trip.service.runsOn(this.date, this.dow)) {
        lastFound = trip;
      }

      if (!lastFound || lastFound === trip) {
        this.routeScanPosition[routeId] = i;
      }
    }

    return lastFound;
  }
}

/**
 * Create the RouteScanner from GTFS trips and calendars
 */
export class RouteScannerFactory {
  constructor(tripsByRoute) {
    this.tripsByRoute = tripsByRoute;
  }

  create(date, dow) {
    return new RouteScanner(this.tripsByRoute, date, dow);
  }
}
