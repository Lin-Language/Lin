import { RaptorAlgorithm } from "./RaptorAlgorithm.js";
import { QueueFactory } from "./QueueFactory.js";
import { RouteScannerFactory } from "./RouteScanner.js";
import { getDateNumber } from "../query/DateUtil.js";
import { ScanResultsFactory } from "./ScanResultsFactory.js";

const DEFAULT_INTERCHANGE_TIME = 0;
const OVERTAKING_ROUTE_SUFFIX = "overtakes";

/**
 * Prepares GTFS data for the raptor algorithm
 */
export class RaptorAlgorithmFactory {
  /**
   * Set up indexes that are required by the Raptor algorithm. If a date is provided all trips will be pre-filtered
   * before being given to the Raptor class.
   */
  static create(trips, transfers, interchange, date) {
    const routesAtStop = {};
    const tripsByRoute = {};
    const routeStopIndex = {};
    const routePath = {};
    const usefulTransfers = {};

    if (date) {
      const dateNumber = getDateNumber(date);
      const dow = date.getDay();

      trips = trips.filter(trip => trip.service.runsOn(dateNumber, dow));
    }

    // Array.prototype.sort is stable since ES2019.
    trips.sort((a, b) => a.stopTimes[0].departureTime - b.stopTimes[0].departureTime);

    for (const trip of trips) {
      const path = trip.stopTimes.map(s => s.stop);
      const routeId = RaptorAlgorithmFactory.getRouteId(trip, tripsByRoute);

      if (!routeStopIndex[routeId]) {
        tripsByRoute[routeId] = [];
        routeStopIndex[routeId] = {};
        routePath[routeId] = path;

        for (let i = path.length - 1; i >= 0; i--) {
          routeStopIndex[routeId][path[i]] = i;
          usefulTransfers[path[i]] = transfers[path[i]] || [];
          interchange[path[i]] = interchange[path[i]] || DEFAULT_INTERCHANGE_TIME;
          routesAtStop[path[i]] = routesAtStop[path[i]] || [];

          if (trip.stopTimes[i].pickUp) {
            routesAtStop[path[i]].push(routeId);
          }
        }
      }

      tripsByRoute[routeId].push(trip);
    }

    return new RaptorAlgorithm(
      routeStopIndex,
      routePath,
      usefulTransfers,
      interchange,
      new ScanResultsFactory(Object.keys(usefulTransfers)),
      new QueueFactory(routesAtStop, routeStopIndex),
      new RouteScannerFactory(tripsByRoute),
    );
  }

  static getRouteId(trip, tripsByRoute) {
    // Array.join() with no arg uses "," as separator. pickUp/dropOff appended as "1"/"0" chars.
    const routeId = trip.stopTimes.map(s => s.stop + (s.pickUp ? 1 : 0) + (s.dropOff ? 1 : 0)).join();

    for (const t of tripsByRoute[routeId] || []) {
      const arrivalTimeA = trip.stopTimes[trip.stopTimes.length - 1].arrivalTime;
      const arrivalTimeB = t.stopTimes[t.stopTimes.length - 1].arrivalTime;

      if (arrivalTimeA < arrivalTimeB) {
        return routeId + OVERTAKING_ROUTE_SUFFIX;
      }
    }

    return routeId;
  }
}
