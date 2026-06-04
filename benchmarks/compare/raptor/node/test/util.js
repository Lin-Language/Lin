import { Service } from "../src/gtfs/Service.js";

export const allDays = { 0: true, 1: true, 2: true, 3: true, 4: true, 5: true, 6: true };

export const services = {
  "1": new Service(
    20180101,
    20991231,
    allDays,
    {}
  ),
  "2": new Service(
    20190101,
    20991231,
    allDays,
    {}
  )
};

let tripId = 0;

export function t(...stopTimes) {
  return {
    tripId: `trip${tripId++}`,
    stopTimes: stopTimes,
    serviceId: "1",
    service: services["1"]
  };
}

export function st(stop, arrivalTime, departureTime) {
  return {
    stop: stop,
    arrivalTime: arrivalTime || departureTime,
    departureTime: departureTime || arrivalTime,
    dropOff: arrivalTime !== null,
    pickUp: departureTime !== null
  };
}

const defaultTrip = { tripId: "1", serviceId: "1", stopTimes: [], service: services["1"] };

export function j(...legStopTimes) {
  return {
    departureTime: getDepartureTime(legStopTimes),
    arrivalTime: getArrivalTime(legStopTimes),
    legs: legStopTimes.map(stopTimes => isTransfer(stopTimes) ? stopTimes : ({
      stopTimes,
      origin: stopTimes[0].stop,
      destination: stopTimes[stopTimes.length - 1].stop,
      trip: defaultTrip
    }))
  };
}

function getDepartureTime(legs) {
  let transferDuration = 0;

  for (const leg of legs) {
    if (isTransfer(leg)) {
      transferDuration += leg.duration;
    }
    else {
      return leg[0].departureTime - transferDuration;
    }
  }

  return 0;
}

function getArrivalTime(legs) {
  let transferDuration = 0;

  for (let i = legs.length - 1; i >= 0; i--) {
    const leg = legs[i];

    if (isTransfer(leg)) {
      transferDuration += leg.duration;
    }
    else {
      return leg[leg.length - 1].arrivalTime + transferDuration;
    }
  }

  return 0;
}

export function isTransfer(connection) {
  return connection.origin !== undefined;
}

export function tf(origin, destination, duration) {
  return { origin, destination, duration, startTime: 0, endTime: Number.MAX_SAFE_INTEGER };
}

/**
 * Overwrite every timetable leg's trip with a fixed default object before comparison, so journey
 * equality ignores trip identity (trap #8/#9).
 */
export function setDefaultTrip(results) {
  for (const trip of results) {
    for (const leg of trip.legs) {
      if (leg.trip) {
        leg.trip = defaultTrip;
      }
    }
  }
}

/**
 * Structural deep equality mirroring vitest's `toEqual`. Handles Sets, arrays, Dates, and plain
 * objects (own enumerable keys, order-independent). Trip identity is already normalised away by
 * setDefaultTrip on the result side and the shared defaultTrip on the j()/expected side, so this is
 * a plain structural compare.
 */
export function deepEqual(a, b) {
  if (a === b) {
    return true;
  }

  if (typeof a !== "object" || typeof b !== "object" || a === null || b === null) {
    // NaN === NaN handling and primitive mismatch
    return a !== a && b !== b;
  }

  if (a instanceof Set || b instanceof Set) {
    if (!(a instanceof Set) || !(b instanceof Set) || a.size !== b.size) {
      return false;
    }
    const bItems = [...b];
    const used = new Array(bItems.length).fill(false);
    for (const av of a) {
      let matched = false;
      for (let i = 0; i < bItems.length; i++) {
        if (!used[i] && deepEqual(av, bItems[i])) {
          used[i] = true;
          matched = true;
          break;
        }
      }
      if (!matched) {
        return false;
      }
    }
    return true;
  }

  if (a instanceof Date || b instanceof Date) {
    return a instanceof Date && b instanceof Date && a.getTime() === b.getTime();
  }

  const aIsArr = Array.isArray(a);
  const bIsArr = Array.isArray(b);
  if (aIsArr !== bIsArr) {
    return false;
  }
  if (aIsArr) {
    if (a.length !== b.length) {
      return false;
    }
    for (let i = 0; i < a.length; i++) {
      if (!deepEqual(a[i], b[i])) {
        return false;
      }
    }
    return true;
  }

  const aKeys = Object.keys(a);
  const bKeys = Object.keys(b);
  if (aKeys.length !== bKeys.length) {
    return false;
  }
  for (const key of aKeys) {
    if (!Object.hasOwn(b, key)) {
      return false;
    }
    if (!deepEqual(a[key], b[key])) {
      return false;
    }
  }
  return true;
}
