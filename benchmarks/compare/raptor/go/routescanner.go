package raptor

// TripsIndexedByRoute maps a route to its trips (sorted by first departure).
type TripsIndexedByRoute = map[RouteID][]*Trip

// RouteFlatDeps holds the flat departure-time matrix for a single route.
type RouteFlatDeps struct {
	Stride int    // number of stops (stride for row-major indexing)
	Deps   []Time // row-major: tripIndex*Stride + stopIndex
}

// RouteDepartures maps a route to its flat departure-time matrix.
type RouteDepartures = map[RouteID]*RouteFlatDeps

// FlatRouteScanner returns the earliest reachable trip index for a route
// using the global integer-indexed flat arrays. Maintains a stateful
// backward-scan position per route across calls within a single scan.
type FlatRouteScanner struct {
	idx          *FlatIndex
	date         DateNumber
	dow          DayOfWeek
	scanPosition []int32 // per route: current scan position (trip index), -1 = unset
	scanPosSet   []bool
}

// GetTrip returns the index of the earliest trip on route ri at stopIndex
// departing at or after `time` (uint32 seconds). Returns -1 if none found.
func (s *FlatRouteScanner) GetTrip(ri uint32, stopIndex uint32, time uint32) int32 {
	if !s.scanPosSet[ri] {
		s.scanPosition[ri] = int32(s.idx.routes[ri].NumTrips) - 1
		s.scanPosSet[ri] = true
	}

	re := s.idx.routes[ri]
	numStops := re.NumStops
	departures := s.idx.departures
	trips := s.idx.routeTrips[ri]
	date := s.date
	dow := s.dow

	var lastFound int32 = -1

	for i := s.scanPosition[ri]; i >= 0; i-- {
		dep := departures[re.StopTimesBase+uint32(i)*numStops+stopIndex]
		if dep < time {
			break
		}
		trip := trips[i]
		if trip.Service.RunsOn(date, dow) {
			lastFound = i
		}
		if lastFound < 0 || lastFound == i {
			s.scanPosition[ri] = i
		}
	}

	return lastFound
}
