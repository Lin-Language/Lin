// Cross-language RAPTOR benchmark — complex queries over the full GTFS feed.
//
// Two workloads, identical across all five language ports:
//   GROUP  — the 24 group-station origin/destination-set queries from the reference
//            test/performance.ts, planned at 10:00 (36000s) on 2025-09-02 with the
//            default MultipleCriteriaFilter (earliestArrival + leastChanges).
//   RANGE  — "next 20 journeys departing after 08:00" profile queries for 5 pairs:
//            repeatedly plan, advance past the earliest departure, until 20 journeys
//            are collected or the service runs out (this is the RangeQuery profile
//            loop capped at N — bounded work, ~<=20 plans per pair).
//
// Output (stdout): per-phase timing lines (the benchmark numbers) and a final
// DIGEST line (the cross-language correctness gate). The digest is order-independent
// (a sum mod a prime), so journey ordering differences never affect it. Every other
// language must reproduce the same GROUP/RANGE journey counts and digests.
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import { loadGTFS } from "./src/gtfs/GTFSLoader.js";
import { RaptorAlgorithmFactory } from "./src/raptor/RaptorAlgorithmFactory.js";
import { GroupStationDepartAfterQuery } from "./src/query/GroupStationDepartAfterQuery.js";
import { JourneyFactory } from "./src/results/JourneyFactory.js";
import { MultipleCriteriaFilter } from "./src/results/filter/MultipleCriteriaFilter.js";

const __dirname = dirname(fileURLToPath(import.meta.url));

const DATE = "2025-09-02";          // Tuesday, in service window
const GROUP_TIME = 36000;           // 10:00, matching performance.ts
const RANGE_START = 28800;          // 08:00
const RANGE_N = 20;                 // "next 20 journeys"
const P = 1000000007n;              // digest modulus (BigInt for exact i64-equivalent math)

// The 24 reference group-station queries (test/performance.ts).
const GROUP_QUERIES = [
  [["MRF", "LVC", "LVJ", "LIV"], ["NRW"]],
  [["TBW", "PDW"], ["HGS"]],
  [["PDW", "MRN"], ["LVC", "LVJ", "LIV"]],
  [["PDW", "AFK"], ["NRW"]],
  [["PDW"], ["BHM", "BMO", "BSW", "BHI"]],
  [["PNZ"], ["DIS"]],
  [["YRK"], ["DIS"]],
  [["WEY"], ["RDG"]],
  [["YRK"], ["NRW"]],
  [["BHM", "BMO", "BSW", "BHI"], ["MCO", "MAN", "MCV", "EXD"]],
  [["BHM", "BMO", "BSW", "BHI"], ["EDB"]],
  [["COV", "RUG"], ["MAN", "MCV"]],
  [["YRK"], ["MCO", "MAN", "MCV", "EXD"]],
  [["STA"], ["PBO"]],
  [["PNZ"], ["EDB"]],
  [["RDG"], ["IPS"]],
  [["DVP"], ["BHM", "BMO", "BSW", "BHI"]],
  [["BXB"], ["DVP"]],
  [["MCO", "MAN", "MCV", "EXD"], ["CBW", "CBE"]],
  [["MCO", "MAN", "MCV", "EXD"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
  [["BHM", "BMO", "BSW", "BHI"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
  [["ORP"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
  [["EDB"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
  [["CBE", "CBW"], ["EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"]],
];

// "next 20 journeys" pairs.
const RANGE_QUERIES = [
  ["TBW", "NRW"],
  ["BHM", "EDB"],
  ["PNZ", "DIS"],
  ["YRK", "NRW"],
  ["RDG", "IPS"],
];

// Order-independent digest contribution of one journey.
function journeyDigest(j) {
  const dep = BigInt(j.departureTime % 1000000000);
  const arr = BigInt(j.arrivalTime % 1000000000);
  const legs = BigInt(j.legs.length);
  return (dep * 1000003n + arr * 31n + legs) % P;
}

function accumulate(journeys, acc) {
  for (const j of journeys) acc = (acc + journeyDigest(j)) % P;
  return acc;
}

// "next N journeys departing after startTime" — the RangeQuery profile loop, capped at N.
function nextN(group, origin, destination, date, startTime, n) {
  const results = [];
  let time = startTime;
  while (results.length < n) {
    const newResults = group.plan([origin], [destination], new Date(date), time);
    if (newResults.length === 0) break;
    results.push(...newResults);
    time = Math.min(...newResults.map(j => j.departureTime)) + 1;
  }
  results.sort((a, b) => a.departureTime - b.departureTime || a.arrivalTime - b.arrivalTime);
  return results.slice(0, n);
}

function main() {
  const dataDir = process.argv[2] || join(__dirname, "..", "data");

  const t0 = performance.now();
  const { trips, transfers, interchange } = loadGTFS(dataDir);
  const loadMs = performance.now() - t0;

  const t1 = performance.now();
  const raptor = RaptorAlgorithmFactory.create(trips, transfers, interchange);
  const prepMs = performance.now() - t1;

  const jf = new JourneyFactory();
  const groupFiltered = new GroupStationDepartAfterQuery(raptor, jf, 3, [new MultipleCriteriaFilter()]);
  const groupPlain = new GroupStationDepartAfterQuery(raptor, jf, 3, []);

  // GROUP workload
  const tg = performance.now();
  let groupCount = 0, groupDigest = 0n;
  for (const [origins, destinations] of GROUP_QUERIES) {
    const results = groupFiltered.plan(origins, destinations, new Date(DATE), GROUP_TIME);
    groupCount += results.length;
    groupDigest = accumulate(results, groupDigest);
  }
  const groupMs = performance.now() - tg;

  // RANGE workload ("next 20")
  const tr = performance.now();
  let rangeCount = 0, rangeDigest = 0n;
  for (const [origin, destination] of RANGE_QUERIES) {
    const results = nextN(groupPlain, origin, destination, DATE, RANGE_START, RANGE_N);
    rangeCount += results.length;
    rangeDigest = accumulate(results, rangeDigest);
  }
  const rangeMs = performance.now() - tr;

  const out = [];
  out.push(`LOAD ms=${loadMs.toFixed(1)}`);
  out.push(`PREP ms=${prepMs.toFixed(1)}`);
  out.push(`GROUP queries=${GROUP_QUERIES.length} journeys=${groupCount} digest=${groupDigest} ms=${groupMs.toFixed(1)}`);
  out.push(`RANGE queries=${RANGE_QUERIES.length} journeys=${rangeCount} digest=${rangeDigest} ms=${rangeMs.toFixed(1)}`);
  out.push(`DIGEST group=${groupDigest} range=${rangeDigest} journeys=${groupCount + rangeCount}`);
  process.stdout.write(out.join("\n") + "\n");
}

main();
