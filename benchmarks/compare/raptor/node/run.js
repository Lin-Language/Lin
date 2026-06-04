import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { loadGTFS } from "./src/gtfs/GTFSLoader.js";
import { RaptorAlgorithmFactory } from "./src/raptor/RaptorAlgorithmFactory.js";
import { DepartAfterQuery } from "./src/query/DepartAfterQuery.js";
import { JourneyFactory } from "./src/results/JourneyFactory.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

/**
 * Format seconds-from-midnight as HH:MM:SS (HH may exceed 24).
 */
function fmt(s) {
  const hh = Math.floor(s / 3600);
  const mm = Math.floor((s % 3600) / 60);
  const ss = s % 60;
  const p = n => String(n).padStart(2, "0");
  return `${p(hh)}:${p(mm)}:${p(ss)}`;
}

function isTimetableLeg(leg) {
  return leg.stopTimes !== undefined;
}

function main() {
  const argv = process.argv.slice(2);
  const dataDir = argv[0] || join(__dirname, "..", "data");
  const origin = argv[1] || "TBW";
  const destination = argv[2] || "NRW";
  const dateStr = argv[3] || "2025-09-02";
  const timeStr = argv[4] || "08:00";

  // HH:MM (or HH:MM:SS) -> seconds from midnight
  const timeParts = timeStr.split(":").map(Number);
  const timeSeconds = (timeParts[0] || 0) * 3600 + (timeParts[1] || 0) * 60 + (timeParts[2] || 0);

  const loadStart = performance.now();
  const { trips, transfers, interchange } = loadGTFS(dataDir);
  const loadMs = performance.now() - loadStart;

  // NO date pre-filter: pass the date through the query (matches DepartAfterQuery).
  const raptor = RaptorAlgorithmFactory.create(trips, transfers, interchange);
  const query = new DepartAfterQuery(raptor, new JourneyFactory());

  const planStart = performance.now();
  const journeys = query.plan(origin, destination, new Date(dateStr), timeSeconds);
  const planMs = performance.now() - planStart;

  process.stderr.write(`load=${loadMs.toFixed(1)}ms plan=${planMs.toFixed(1)}ms\n`);

  // Sort by departureTime asc then arrivalTime asc for stable cross-language output.
  journeys.sort((a, b) => a.departureTime - b.departureTime || a.arrivalTime - b.arrivalTime);

  const out = [];
  for (const journey of journeys) {
    out.push(`JOURNEY dep=${fmt(journey.departureTime)} arr=${fmt(journey.arrivalTime)} legs=${journey.legs.length}`);
    for (const leg of journey.legs) {
      if (isTimetableLeg(leg)) {
        const first = leg.stopTimes[0];
        const last = leg.stopTimes[leg.stopTimes.length - 1];
        out.push(`  ${first.stop} ${fmt(first.departureTime)} -> ${last.stop} ${fmt(last.arrivalTime)}`);
      } else {
        out.push(`  TRANSFER ${leg.origin} -> ${leg.destination} (${leg.duration}s)`);
      }
    }
  }

  // RESULT: from the journey with the earliest arrival (ties: fewest legs).
  let best = null;
  for (const journey of journeys) {
    if (
      best === null ||
      journey.arrivalTime < best.arrivalTime ||
      (journey.arrivalTime === best.arrivalTime && journey.legs.length < best.legs.length)
    ) {
      best = journey;
    }
  }

  if (best !== null) {
    out.push(`RESULT dep=${best.departureTime} arr=${best.arrivalTime} legs=${best.legs.length} count=${journeys.length}`);
  } else {
    out.push(`RESULT dep=0 arr=0 legs=0 count=0`);
  }

  process.stdout.write(out.join("\n") + "\n");
}

main();
