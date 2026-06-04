package raptor

// TripsIndexedByRoute maps a route to its trips (sorted by first departure).
type TripsIndexedByRoute = map[RouteID][]*Trip

// RouteScanner returns the earliest reachable trip for a route, maintaining a
// stateful backward-scan position memo across calls within a single scan.
type RouteScanner struct {
	tripsByRoute      TripsIndexedByRoute
	date              DateNumber
	dow               DayOfWeek
	routeScanPosition map[RouteID]int
	scanPosSet        map[RouteID]bool
}

// GetTrip returns the earliest trip stop times possible on the given route at
// or after the given time, scanning backward from the last found position.
func (s *RouteScanner) GetTrip(routeId RouteID, stopIndex int, time Time) *Trip {
	if !s.scanPosSet[routeId] {
		s.routeScanPosition[routeId] = len(s.tripsByRoute[routeId]) - 1
		s.scanPosSet[routeId] = true
	}

	var lastFound *Trip
	routeTrips := s.tripsByRoute[routeId]

	for i := s.routeScanPosition[routeId]; i >= 0; i-- {
		trip := routeTrips[i]
		stopTime := trip.StopTimes[stopIndex]

		if stopTime.DepartureTime < time {
			break
		} else if trip.Service.RunsOn(s.date, s.dow) {
			lastFound = trip
		}

		if lastFound == nil || lastFound == trip {
			s.routeScanPosition[routeId] = i
		}
	}

	return lastFound
}

// RouteScannerFactory creates a fresh RouteScanner per scan (per day).
type RouteScannerFactory struct {
	tripsByRoute TripsIndexedByRoute
}

// NewRouteScannerFactory constructs the factory.
func NewRouteScannerFactory(tripsByRoute TripsIndexedByRoute) *RouteScannerFactory {
	return &RouteScannerFactory{tripsByRoute: tripsByRoute}
}

// Create builds a RouteScanner for a specific date / day-of-week.
func (f *RouteScannerFactory) Create(date DateNumber, dow DayOfWeek) *RouteScanner {
	return &RouteScanner{
		tripsByRoute:      f.tripsByRoute,
		date:              date,
		dow:               dow,
		routeScanPosition: map[RouteID]int{},
		scanPosSet:        map[RouteID]bool{},
	}
}
