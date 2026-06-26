import { RaptorAlgorithm } from "./RaptorAlgorithm.js";
import { QueueFactory } from "./QueueFactory.js";
import { RouteScannerFactory } from "./RouteScanner.js";
import { getDateNumber } from "../query/DateUtil.js";
import { ScanResultsFactory } from "./ScanResultsFactory.js";

const DEFAULT_INTERCHANGE_TIME = 0;
const OVERTAKING_ROUTE_SUFFIX = "_OVR";

/**
 * Prepares GTFS data for the raptor algorithm.
 *
 * Integer-indexed global-array layout:
 *   - stopIndexOf: string stopId -> int index
 *   - stopNames:   int index -> string stopId
 *   - routeId:     signature string -> int index
 *   - routes[r]:   { stopTimesBase, numStops, numTrips, routeStopsBase }
 *   - routeStops:  flat Int32Array, route r's stops at [routeStopsBase, +numStops]
 *   - arrivals/departures: global flat Int32Array, trip-major per route
 *       entry for route r, trip t, stop s = stopTimesBase + t*numStops + s
 *   - routeTrips[r]: Trip[] for service-day checks and journey reconstruction
 *   - stopRoutes:  flat Int32Array, inverse index of routes at each stop
 *   - stopRoutesBase: Int32Array[numStops], offset into stopRoutes for each stop
 *   - stopRoutesCount: Int32Array[numStops], # routes at each stop
 *   - interchange: Int32Array[numStops], min interchange in seconds
 *   - transfers[stopIdx]: array of transfer objects (dest as int)
 */
export class RaptorAlgorithmFactory {
  static create(trips, transfers, interchange, date) {
    if (date) {
      const dateNumber = getDateNumber(date);
      const dow = date.getDay();
      trips = trips.filter(trip => trip.service.runsOn(dateNumber, dow));
    }

    // --- Pass 0: intern stop ids ---
    const stopIndexOf = new Map();   // string -> int
    const stopNames = [];            // int -> string

    function internStop(s) {
      let idx = stopIndexOf.get(s);
      if (idx === undefined) {
        idx = stopNames.length;
        stopIndexOf.set(s, idx);
        stopNames.push(s);
      }
      return idx;
    }

    // Pre-intern all stops from trips (maintains visit order from sorted trips)
    trips.sort((a, b) => a.stopTimes[0].departureTime - b.stopTimes[0].departureTime);

    for (const trip of trips) {
      for (const st of trip.stopTimes) {
        internStop(st.stop);
      }
    }

    // Also intern stops from transfers (may have stops not in any trip)
    for (const fromStop of Object.keys(transfers)) {
      internStop(fromStop);
      for (const t of transfers[fromStop]) {
        internStop(t.destination);
      }
    }

    const numStops = stopNames.length;

    // --- Pass 1: group trips by route signature, assign integer route ids ---
    const routeSigToId = new Map();  // signature -> int
    const routeSigs = [];            // int -> signature string (for dedup)

    // Per-route accumulators (keyed by int route id)
    const routeNumStops = [];        // int route id -> numStops
    const routeTripsList = [];       // int route id -> Trip[] (in departure order)
    const routeStopSeq = [];         // int route id -> int[] stop sequence
    // Per-stop: set of int route ids (for pickUp stops)
    const stopRouteSet = [];
    for (let i = 0; i < numStops; i++) stopRouteSet.push(new Set());

    // routeStopIndex[r][localStopIdx] = position in route — for marking earliest stop
    // We build this as we go, storing as array indexed by local stop position.
    const routeStopPos = [];  // int route id -> Map<stopIdx, pos>

    function getRouteId(trip) {
      // Signature: stop indices + pickUp/dropOff bits
      const parts = trip.stopTimes.map(s =>
        internStop(s.stop) + "," + (s.pickUp ? 1 : 0) + "," + (s.dropOff ? 1 : 0)
      );
      let sig = parts.join("|");

      // Check overtaking
      const existingId = routeSigToId.get(sig);
      if (existingId !== undefined) {
        // Check if this trip overtakes any already-assigned trip
        const lastArrA = trip.stopTimes[trip.stopTimes.length - 1].arrivalTime;
        for (const t of routeTripsList[existingId]) {
          const lastArrB = t.stopTimes[t.stopTimes.length - 1].arrivalTime;
          if (lastArrA < lastArrB) {
            sig = sig + OVERTAKING_ROUTE_SUFFIX;
            break;
          }
        }
      }

      let id = routeSigToId.get(sig);
      if (id === undefined) {
        id = routeSigs.length;
        routeSigToId.set(sig, id);
        routeSigs.push(sig);
        routeNumStops.push(trip.stopTimes.length);
        routeTripsList.push([]);
        const stopSeq = trip.stopTimes.map(s => internStop(s.stop));
        routeStopSeq.push(stopSeq);
        const posMap = new Map();
        for (let i = 0; i < stopSeq.length; i++) posMap.set(stopSeq[i], i);
        routeStopPos.push(posMap);

        // Register routes at stops (pickup only)
        for (let i = 0; i < trip.stopTimes.length; i++) {
          if (trip.stopTimes[i].pickUp) {
            stopRouteSet[stopSeq[i]].add(id);
          }
        }
      }

      routeTripsList[id].push(trip);
      return id;
    }

    // Assign all trips to routes (sorted order already ensures trip ordering within route)
    for (const trip of trips) {
      getRouteId(trip);
    }

    const numRoutes = routeSigs.length;

    // --- Pass 2: build global flat typed arrays ---

    // routeStops: flat Int32Array of stop sequences concatenated
    let totalRouteStops = 0;
    for (let r = 0; r < numRoutes; r++) totalRouteStops += routeNumStops[r];
    const routeStops = new Int32Array(totalRouteStops);

    // arrivals + departures: global, trip-major per route
    // Total size = sum over routes of (numTrips * numStops)
    let totalStopTimes = 0;
    const routes = new Array(numRoutes);
    {
      let rsBase = 0, stBase = 0;
      for (let r = 0; r < numRoutes; r++) {
        const ns = routeNumStops[r];
        const nt = routeTripsList[r].length;
        routes[r] = { stopTimesBase: stBase, numStops: ns, numTrips: nt, routeStopsBase: rsBase };
        // Fill routeStops slice
        const seq = routeStopSeq[r];
        for (let i = 0; i < ns; i++) routeStops[rsBase + i] = seq[i];
        rsBase += ns;
        stBase += nt * ns;
        totalStopTimes += nt * ns;
      }
    }

    const arrivals = new Int32Array(totalStopTimes);
    const departures = new Int32Array(totalStopTimes);

    for (let r = 0; r < numRoutes; r++) {
      const { stopTimesBase, numStops: ns } = routes[r];
      const tripsForRoute = routeTripsList[r];
      for (let t = 0; t < tripsForRoute.length; t++) {
        const trip = tripsForRoute[t];
        const base = stopTimesBase + t * ns;
        for (let s = 0; s < ns; s++) {
          arrivals[base + s] = trip.stopTimes[s].arrivalTime;
          departures[base + s] = trip.stopTimes[s].departureTime;
        }
      }
    }

    // --- Build stopRoutes (inverse index) ---
    // stopRouteSet[stopIdx] = Set<routeIdx> of routes serving that stop with pickUp
    const stopRoutesCount = new Int32Array(numStops);
    for (let s = 0; s < numStops; s++) {
      stopRoutesCount[s] = stopRouteSet[s].size;
    }

    const stopRoutesBase = new Int32Array(numStops);
    let srTotal = 0;
    for (let s = 0; s < numStops; s++) {
      stopRoutesBase[s] = srTotal;
      srTotal += stopRoutesCount[s];
    }
    const stopRoutes = new Int32Array(srTotal);
    const srFill = new Int32Array(numStops);
    // Fill stopRoutes: for each stop, write the route ids that serve it
    for (let s = 0; s < numStops; s++) {
      for (const r of stopRouteSet[s]) {
        const offset = stopRoutesBase[s] + srFill[s];
        stopRoutes[offset] = r;
        srFill[s]++;
      }
    }

    // --- Build interchange typed array ---
    const interchangeArr = new Int32Array(numStops);
    for (let s = 0; s < numStops; s++) {
      const name = stopNames[s];
      interchangeArr[s] = interchange[name] !== undefined ? interchange[name] : DEFAULT_INTERCHANGE_TIME;
    }

    // --- Build transfers with integer indices ---
    // transfersArr[stopIdx] = [{destIdx, duration, startTime, endTime, origin, destination}]
    // We keep origin/destination strings for journey reconstruction but add int dest index.
    const transfersArr = new Array(numStops).fill(null).map(() => []);
    for (const fromStop of Object.keys(transfers)) {
      const fromIdx = stopIndexOf.get(fromStop);
      if (fromIdx === undefined) continue;
      for (const t of transfers[fromStop]) {
        const destIdx = stopIndexOf.get(t.destination);
        if (destIdx === undefined) continue;
        transfersArr[fromIdx].push({
          destIdx,
          duration: t.duration,
          startTime: t.startTime,
          endTime: t.endTime,
          origin: t.origin,
          destination: t.destination,
        });
      }
    }

    return new RaptorAlgorithm(
      numStops,
      numRoutes,
      stopIndexOf,
      stopNames,
      routes,
      routeStops,
      arrivals,
      departures,
      routeTripsList,
      routeStopPos,
      stopRoutes,
      stopRoutesBase,
      stopRoutesCount,
      interchangeArr,
      transfersArr,
      new ScanResultsFactory(numStops, stopIndexOf, stopNames),
      new QueueFactory(numStops, stopRoutes, stopRoutesBase, stopRoutesCount, routeStopPos),
      new RouteScannerFactory(numRoutes, routeTripsList, arrivals, departures, routes),
    );
  }
}
