// Command run loads the GTFS feed, builds the RAPTOR algorithm and plans a
// single departAfter journey, printing the contract output format to stdout and
// timing to stderr.
//
// Usage:
//
//	go run ./cmd/run [dataDir] [origin] [destination] [YYYY-MM-DD] [HH:MM]
//
// Defaults: dataDir=../data (relative to go/, i.e. run from the go/ dir),
// TBW -> NRW, 2025-09-02, 08:00.
package main

import (
	"fmt"
	"os"
	"sort"
	"strconv"
	"strings"
	"time"

	"raptor"
)

// fmtTime formats seconds-from-midnight as HH:MM:SS (HH may exceed 24).
func fmtTime(s raptor.Time) string {
	hh := s / 3600
	mm := (s % 3600) / 60
	ss := s % 60
	return fmt.Sprintf("%02d:%02d:%02d", hh, mm, ss)
}

func main() {
	args := os.Args[1:]
	arg := func(i int, def string) string {
		if i < len(args) {
			return args[i]
		}
		return def
	}

	dataDir := arg(0, "../data")
	origin := arg(1, "TBW")
	destination := arg(2, "NRW")
	dateStr := arg(3, "2025-09-02")
	timeStr := arg(4, "08:00")

	// HH:MM (or HH:MM:SS) -> seconds from midnight.
	var timeSeconds raptor.Time
	for i, p := range strings.Split(timeStr, ":") {
		v, _ := strconv.ParseInt(p, 10, 64)
		switch i {
		case 0:
			timeSeconds += v * 3600
		case 1:
			timeSeconds += v * 60
		case 2:
			timeSeconds += v
		}
	}

	loadStart := time.Now()
	trips, transfers, interchange, err := raptor.LoadGTFS(dataDir)
	if err != nil {
		fmt.Fprintf(os.Stderr, "load error: %v\n", err)
		os.Exit(1)
	}
	loadMs := float64(time.Since(loadStart).Microseconds()) / 1000

	// NO date pre-filter: pass the date through the query (matches DepartAfterQuery).
	r := raptor.CreateRaptorAlgorithm(trips, transfers, interchange, nil)
	query := raptor.NewDepartAfterQuery(r, raptor.NewJourneyFactory(), 3)

	date := raptor.NewSearchDate(dateStr)

	planStart := time.Now()
	journeys := query.Plan(origin, destination, date, timeSeconds)
	planMs := float64(time.Since(planStart).Microseconds()) / 1000

	fmt.Fprintf(os.Stderr, "load=%.1fms plan=%.1fms\n", loadMs, planMs)

	// Sort by departureTime asc then arrivalTime asc for stable cross-language output.
	sort.SliceStable(journeys, func(i, j int) bool {
		if journeys[i].DepartureTime != journeys[j].DepartureTime {
			return journeys[i].DepartureTime < journeys[j].DepartureTime
		}
		return journeys[i].ArrivalTime < journeys[j].ArrivalTime
	})

	var out []string
	for _, journey := range journeys {
		out = append(out, fmt.Sprintf("JOURNEY dep=%s arr=%s legs=%d",
			fmtTime(journey.DepartureTime), fmtTime(journey.ArrivalTime), len(journey.Legs)))
		for _, leg := range journey.Legs {
			switch l := leg.(type) {
			case *raptor.TimetableLeg:
				first := l.StopTimes[0]
				last := l.StopTimes[len(l.StopTimes)-1]
				out = append(out, fmt.Sprintf("  %s %s -> %s %s",
					first.Stop, fmtTime(first.DepartureTime), last.Stop, fmtTime(last.ArrivalTime)))
			case *raptor.Transfer:
				out = append(out, fmt.Sprintf("  TRANSFER %s -> %s (%ds)",
					l.Origin, l.Destination, l.Duration))
			}
		}
	}

	// RESULT: from the journey with the earliest arrival (ties: fewest legs).
	var best *raptor.Journey
	for i := range journeys {
		j := &journeys[i]
		if best == nil ||
			j.ArrivalTime < best.ArrivalTime ||
			(j.ArrivalTime == best.ArrivalTime && len(j.Legs) < len(best.Legs)) {
			best = j
		}
	}

	if best != nil {
		out = append(out, fmt.Sprintf("RESULT dep=%d arr=%d legs=%d count=%d",
			best.DepartureTime, best.ArrivalTime, len(best.Legs), len(journeys)))
	} else {
		out = append(out, "RESULT dep=0 arr=0 legs=0 count=0")
	}

	fmt.Println(strings.Join(out, "\n"))
}
