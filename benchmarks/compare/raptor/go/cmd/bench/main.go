// Command bench runs the cross-language RAPTOR benchmark — two workloads (GROUP
// and RANGE) over the full GTFS feed — and prints per-phase timings plus the
// cross-language correctness DIGEST line.
//
// It mirrors node/bench.js exactly: same GROUP_QUERIES list, same RANGE pairs,
// same journeyDigest formula and accumulation, so the GROUP/RANGE journey
// counts and digests must match byte-for-byte across every language port. Only
// the timing (ms) values differ.
//
// Usage:
//
//	go run ./cmd/bench [dataDir]
//
// Default dataDir=../data (relative to go/, matching cmd/run).
package main

import (
	"fmt"
	"os"
	"sort"
	"time"

	"raptor"
)

const (
	dateStr    = "2025-09-02" // Tuesday, in service window
	groupTime  = 36000        // 10:00, matching performance.ts
	rangeStart = 28800        // 08:00
	rangeN     = 20           // "next 20 journeys"
	digestMod  = 1000000007   // digest modulus
)

// The 24 reference group-station queries (test/performance.ts), matching
// node/bench.js GROUP_QUERIES exactly.
var groupQueries = [][2][]raptor.StopID{
	{{"MRF", "LVC", "LVJ", "LIV"}, {"NRW"}},
	{{"TBW", "PDW"}, {"HGS"}},
	{{"PDW", "MRN"}, {"LVC", "LVJ", "LIV"}},
	{{"PDW", "AFK"}, {"NRW"}},
	{{"PDW"}, {"BHM", "BMO", "BSW", "BHI"}},
	{{"PNZ"}, {"DIS"}},
	{{"YRK"}, {"DIS"}},
	{{"WEY"}, {"RDG"}},
	{{"YRK"}, {"NRW"}},
	{{"BHM", "BMO", "BSW", "BHI"}, {"MCO", "MAN", "MCV", "EXD"}},
	{{"BHM", "BMO", "BSW", "BHI"}, {"EDB"}},
	{{"COV", "RUG"}, {"MAN", "MCV"}},
	{{"YRK"}, {"MCO", "MAN", "MCV", "EXD"}},
	{{"STA"}, {"PBO"}},
	{{"PNZ"}, {"EDB"}},
	{{"RDG"}, {"IPS"}},
	{{"DVP"}, {"BHM", "BMO", "BSW", "BHI"}},
	{{"BXB"}, {"DVP"}},
	{{"MCO", "MAN", "MCV", "EXD"}, {"CBW", "CBE"}},
	{{"MCO", "MAN", "MCV", "EXD"}, {"EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"}},
	{{"BHM", "BMO", "BSW", "BHI"}, {"EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"}},
	{{"ORP"}, {"EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"}},
	{{"EDB"}, {"EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"}},
	{{"CBE", "CBW"}, {"EUS", "MYB", "STP", "PAD", "BFR", "CTK", "CST", "CHX", "LBG", "WAE", "VIC", "VXH", "WAT", "OLD", "MOG", "KGX", "LST", "FST"}},
}

// "next 20 journeys" pairs, matching node/bench.js RANGE_QUERIES.
var rangeQueries = [][2]raptor.StopID{
	{"TBW", "NRW"},
	{"BHM", "EDB"},
	{"PNZ", "DIS"},
	{"YRK", "NRW"},
	{"RDG", "IPS"},
}

// journeyDigest returns the order-independent digest contribution of one
// journey, matching node/bench.js journeyDigest. The intermediate products fit
// in uint64 (dep,arr < 1e9, *1000003 < 1e15), so no overflow.
func journeyDigest(j raptor.Journey) uint64 {
	dep := uint64(j.DepartureTime % 1000000000)
	arr := uint64(j.ArrivalTime % 1000000000)
	legs := uint64(len(j.Legs))
	return (dep*1000003 + arr*31 + legs) % digestMod
}

func accumulate(journeys []raptor.Journey, acc uint64) uint64 {
	for _, j := range journeys {
		acc = (acc + journeyDigest(j)) % digestMod
	}
	return acc
}

// nextN reproduces node/bench.js nextN: "next n journeys departing after
// startTime" — repeatedly plan with no filter, advance past the earliest
// departure, until n collected or service runs out, then sort and slice.
func nextN(group *raptor.GroupStationDepartAfterQuery, origin, destination raptor.StopID, date raptor.SearchDate, startTime raptor.Time, n int) []raptor.Journey {
	results := []raptor.Journey{}
	t := startTime
	for len(results) < n {
		newResults := group.Plan([]raptor.StopID{origin}, []raptor.StopID{destination}, date, t)
		if len(newResults) == 0 {
			break
		}
		results = append(results, newResults...)
		min := newResults[0].DepartureTime
		for _, j := range newResults[1:] {
			if j.DepartureTime < min {
				min = j.DepartureTime
			}
		}
		t = min + 1
	}
	sort.SliceStable(results, func(i, j int) bool {
		if results[i].DepartureTime != results[j].DepartureTime {
			return results[i].DepartureTime < results[j].DepartureTime
		}
		return results[i].ArrivalTime < results[j].ArrivalTime
	})
	if len(results) > n {
		results = results[:n]
	}
	return results
}

func main() {
	dataDir := "../data"
	if len(os.Args) > 1 {
		dataDir = os.Args[1]
	}

	t0 := time.Now()
	trips, transfers, interchange, err := raptor.LoadGTFS(dataDir)
	if err != nil {
		fmt.Fprintf(os.Stderr, "load error: %v\n", err)
		os.Exit(1)
	}
	loadMs := float64(time.Since(t0).Microseconds()) / 1000

	t1 := time.Now()
	// NO date pre-filter: pass the date through the query (matches bench.js).
	r := raptor.CreateRaptorAlgorithm(trips, transfers, interchange, nil)
	prepMs := float64(time.Since(t1).Microseconds()) / 1000

	jf := raptor.NewJourneyFactory()
	groupFiltered := raptor.NewGroupStationDepartAfterQuery(r, jf, 3, []raptor.JourneyFilter{raptor.NewMultipleCriteriaFilter()})
	groupPlain := raptor.NewGroupStationDepartAfterQuery(r, jf, 3, nil)

	date := raptor.NewSearchDate(dateStr)

	// GROUP workload
	tg := time.Now()
	groupCount := 0
	var groupDigestAcc uint64
	for _, q := range groupQueries {
		results := groupFiltered.Plan(q[0], q[1], date, groupTime)
		groupCount += len(results)
		groupDigestAcc = accumulate(results, groupDigestAcc)
	}
	groupMs := float64(time.Since(tg).Microseconds()) / 1000

	// RANGE workload ("next 20")
	tr := time.Now()
	rangeCount := 0
	var rangeDigestAcc uint64
	for _, q := range rangeQueries {
		results := nextN(groupPlain, q[0], q[1], date, rangeStart, rangeN)
		rangeCount += len(results)
		rangeDigestAcc = accumulate(results, rangeDigestAcc)
	}
	rangeMs := float64(time.Since(tr).Microseconds()) / 1000

	fmt.Printf("LOAD ms=%.1f\n", loadMs)
	fmt.Printf("PREP ms=%.1f\n", prepMs)
	fmt.Printf("GROUP queries=%d journeys=%d digest=%d ms=%.1f\n", len(groupQueries), groupCount, groupDigestAcc, groupMs)
	fmt.Printf("RANGE queries=%d journeys=%d digest=%d ms=%.1f\n", len(rangeQueries), rangeCount, rangeDigestAcc, rangeMs)
	fmt.Printf("DIGEST group=%d range=%d journeys=%d\n", groupDigestAcc, rangeDigestAcc, groupCount+rangeCount)
}
