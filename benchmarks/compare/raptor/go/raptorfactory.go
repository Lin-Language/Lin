package raptor

import "sort"

const overtakingRouteSuffix = "overtakes"

// CreateRaptorAlgorithm prepares GTFS data for the algorithm. If date is
// non-nil, trips are pre-filtered to those running on that date.
func CreateRaptorAlgorithm(trips []*Trip, transfers TransfersByOrigin, interchange Interchange, date *SearchDate) *RaptorAlgorithm {
	if transfers == nil {
		transfers = TransfersByOrigin{}
	}
	if interchange == nil {
		interchange = Interchange{}
	}

	routesAtStop := RoutesIndexedByStop{}
	tripsByRoute := TripsIndexedByRoute{}
	routeStopIndex := RouteStopIndex{}
	routePath := RoutePaths{}
	// usefulTransfers order defines the scan's stop universe; preserve insertion
	// order to match Object.keys(usefulTransfers).
	usefulTransfersOrder := []StopID{}
	usefulTransfersSeen := map[StopID]bool{}
	usefulTransfers := TransfersByOrigin{}

	if date != nil {
		dateNumber := date.GetDateNumber()
		dow := date.DayOfWeek()
		filtered := trips[:0:0]
		for _, t := range trips {
			if t.Service.RunsOn(dateNumber, dow) {
				filtered = append(filtered, t)
			}
		}
		trips = filtered
	}

	// Stable sort by first departure time (JS Array.sort is stable since ES2019).
	sorted := make([]*Trip, len(trips))
	copy(sorted, trips)
	sort.SliceStable(sorted, func(i, j int) bool {
		return sorted[i].StopTimes[0].DepartureTime < sorted[j].StopTimes[0].DepartureTime
	})

	for _, trip := range sorted {
		path := make([]StopID, len(trip.StopTimes))
		for i, s := range trip.StopTimes {
			path[i] = s.Stop
		}
		routeId := getRouteId(trip, tripsByRoute)

		if _, ok := routeStopIndex[routeId]; !ok {
			tripsByRoute[routeId] = []*Trip{}
			routeStopIndex[routeId] = map[StopID]int{}
			routePath[routeId] = path

			for i := len(path) - 1; i >= 0; i-- {
				stop := path[i]
				routeStopIndex[routeId][stop] = i

				if !usefulTransfersSeen[stop] {
					usefulTransfersSeen[stop] = true
					usefulTransfersOrder = append(usefulTransfersOrder, stop)
				}
				if t, ok := transfers[stop]; ok {
					usefulTransfers[stop] = t
				} else {
					usefulTransfers[stop] = []*Transfer{}
				}

				if _, ok := interchange[stop]; !ok {
					interchange[stop] = 0
				}
				if _, ok := routesAtStop[stop]; !ok {
					routesAtStop[stop] = []RouteID{}
				}

				if trip.StopTimes[i].PickUp {
					routesAtStop[stop] = append(routesAtStop[stop], routeId)
				}
			}
		}

		tripsByRoute[routeId] = append(tripsByRoute[routeId], trip)
	}

	return &RaptorAlgorithm{
		routeStopIndex:      routeStopIndex,
		routePath:           routePath,
		transfers:           usefulTransfers,
		interchange:         interchange,
		scanResultsFactory:  NewScanResultsFactory(usefulTransfersOrder),
		queueFactory:        NewQueueFactory(routesAtStop, routeStopIndex),
		routeScannerFactory: NewRouteScannerFactory(tripsByRoute),
	}
}

// getRouteId builds the route signature string:
//
//	trip.stopTimes.map(s => s.stop + (pickUp?1:0) + (dropOff?1:0)).join(",")
//
// appending "overtakes" when an earlier trip on the same routeId arrives later.
func getRouteId(trip *Trip, tripsByRoute TripsIndexedByRoute) RouteID {
	routeId := ""
	for i, s := range trip.StopTimes {
		if i > 0 {
			routeId += ","
		}
		routeId += s.Stop + boolDigit(s.PickUp) + boolDigit(s.DropOff)
	}

	arrivalTimeA := trip.StopTimes[len(trip.StopTimes)-1].ArrivalTime
	for _, t := range tripsByRoute[routeId] {
		arrivalTimeB := t.StopTimes[len(t.StopTimes)-1].ArrivalTime
		if arrivalTimeA < arrivalTimeB {
			return routeId + overtakingRouteSuffix
		}
	}

	return routeId
}

func boolDigit(b bool) string {
	if b {
		return "1"
	}
	return "0"
}
