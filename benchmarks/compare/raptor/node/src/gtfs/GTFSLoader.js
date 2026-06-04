import { readFileSync } from "node:fs";
import { join } from "node:path";
import { TimeParser } from "./TimeParser.js";
import { Service } from "./Service.js";

const MAX_SAFE_INTEGER = Number.MAX_SAFE_INTEGER;

/**
 * Read a plain CSV file (no quoted fields in this feed) and return { header, rows }
 * where header is the array of column names and rows is an array of string arrays.
 *
 * Trailing empty line(s) are skipped. A simple split-on-comma is sufficient.
 */
function readCsv(path) {
  const text = readFileSync(path, "utf8");
  const lines = text.split("\n");
  const header = lines[0].split(",");
  const rows = [];

  for (let i = 1; i < lines.length; i++) {
    const line = lines[i];
    if (line.length === 0) {
      continue;
    }
    rows.push(line.split(","));
  }

  return { header, rows };
}

/**
 * Build a column-name -> index lookup so we read columns by name (like the
 * reference's row.<field>), independent of incidental column order.
 */
function indexOfColumns(header) {
  const idx = {};
  for (let i = 0; i < header.length; i++) {
    idx[header[i]] = i;
  }
  return idx;
}

/**
 * Returns [trips, transfers, interchange] from a directory of GTFS CSV files.
 *
 * Mirrors GTFSLoader.ts: trips with stopTimes in file order; Service built from
 * calendar + calendar_dates; transfers from transfers.txt (same-stop -> interchange,
 * else a transfer) plus links.txt footpaths. Trips whose serviceId has no calendar
 * row are dropped and the count reported to stderr.
 */
export function loadGTFS(dataDir) {
  const timeParser = new TimeParser();
  const trips = [];
  const transfers = {};
  const interchange = {};
  const calendars = {};
  const dates = {};
  const stopTimes = {};

  // --- calendar.txt -> calendars ---
  {
    const { header, rows } = readCsv(join(dataDir, "calendar.txt"));
    const c = indexOfColumns(header);
    for (const row of rows) {
      const serviceId = row[c.service_id];
      calendars[serviceId] = {
        serviceId,
        startDate: +row[c.start_date],
        endDate: +row[c.end_date],
        days: {
          0: row[c.sunday] === "1",
          1: row[c.monday] === "1",
          2: row[c.tuesday] === "1",
          3: row[c.wednesday] === "1",
          4: row[c.thursday] === "1",
          5: row[c.friday] === "1",
          6: row[c.saturday] === "1",
        },
      };
    }
  }

  // --- calendar_dates.txt -> dates[service_id][+date] = (exception_type === "1") ---
  {
    const { header, rows } = readCsv(join(dataDir, "calendar_dates.txt"));
    const c = indexOfColumns(header);
    for (const row of rows) {
      const serviceId = row[c.service_id];
      const date = row[c.date];
      const include = row[c.exception_type] === "1";

      if (dates[serviceId] === undefined) {
        dates[serviceId] = {};
      }
      dates[serviceId][+date] = include;
    }
  }

  // --- stop_times.txt -> stopTimes grouped by trip_id, in file order ---
  {
    const { header, rows } = readCsv(join(dataDir, "stop_times.txt"));
    const c = indexOfColumns(header);
    for (const row of rows) {
      const tripId = row[c.trip_id];
      const pickupType = row[c.pickup_type];
      const dropOffType = row[c.drop_off_type];

      const stopTime = {
        stop: row[c.stop_id],
        departureTime: timeParser.getTime(row[c.departure_time]),
        arrivalTime: timeParser.getTime(row[c.arrival_time]),
        // "0" or empty/undefined => true; "1"/"3" => false (matches reference).
        pickUp: pickupType === "0" || pickupType === "" || pickupType === undefined,
        dropOff: dropOffType === "0" || dropOffType === "" || dropOffType === undefined,
      };

      let list = stopTimes[tripId];
      if (list === undefined) {
        list = [];
        stopTimes[tripId] = list;
      }
      list.push(stopTime);
    }
  }

  // --- trips.txt -> trips (stopTimes/service resolved after) ---
  {
    const { header, rows } = readCsv(join(dataDir, "trips.txt"));
    const c = indexOfColumns(header);
    for (const row of rows) {
      trips.push({
        serviceId: row[c.service_id],
        tripId: row[c.trip_id],
        stopTimes: [],
        service: {},
      });
    }
  }

  // --- transfers.txt -> interchange + transfers ---
  {
    const { header, rows } = readCsv(join(dataDir, "transfers.txt"));
    const c = indexOfColumns(header);
    for (const row of rows) {
      const from = row[c.from_stop_id];
      const to = row[c.to_stop_id];

      if (from === to) {
        interchange[from] = +row[c.min_transfer_time];
      } else {
        const t = {
          origin: from,
          destination: to,
          duration: +row[c.min_transfer_time],
          startTime: 0,
          endTime: MAX_SAFE_INTEGER,
        };

        if (transfers[from] === undefined) {
          transfers[from] = [];
        }
        transfers[from].push(t);
      }
    }
  }

  // --- links.txt -> transfers (footpaths; date/day columns ignored, matching reference) ---
  {
    const { header, rows } = readCsv(join(dataDir, "links.txt"));
    const c = indexOfColumns(header);
    for (const row of rows) {
      const from = row[c.from_stop_id];
      const t = {
        origin: from,
        destination: row[c.to_stop_id],
        duration: +row[c.duration],
        startTime: timeParser.getTime(row[c.start_time]),
        endTime: timeParser.getTime(row[c.end_time]),
      };

      if (transfers[from] === undefined) {
        transfers[from] = [];
      }
      transfers[from].push(t);
    }
  }

  // --- Service resolution ---
  const services = {};
  for (const serviceId of Object.keys(calendars)) {
    const cal = calendars[serviceId];
    services[serviceId] = new Service(cal.startDate, cal.endDate, cal.days, dates[serviceId] || {});
  }

  // Resolve stopTimes + service per trip; drop trips whose serviceId has no calendar.
  const resolvedTrips = [];
  let dropped = 0;
  for (const t of trips) {
    const service = services[t.serviceId];
    if (service === undefined) {
      dropped++;
      continue;
    }
    t.stopTimes = stopTimes[t.tripId] || [];
    t.service = service;
    resolvedTrips.push(t);
  }

  if (dropped > 0) {
    process.stderr.write(`dropped ${dropped} trip(s) with no calendar row\n`);
  }

  return { trips: resolvedTrips, transfers, interchange };
}
