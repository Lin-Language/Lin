package raptor

// RoutePaths maps a route to its ordered list of stops.
type RoutePaths = map[RouteID][]StopID

// Interchange maps a stop to its interchange time.
type Interchange = map[StopID]Time

// TransfersByOrigin maps a stop to the transfers originating there.
type TransfersByOrigin = map[StopID][]*Transfer

// StopTimes maps a stop to a departure time (origin departure index). It uses
// an insertion-ordered map because the scan's initial marked stops are
// Object.keys(origins) and that order is load-bearing for output ordering.
type StopTimes = *OrderedMap[Time]

// RaptorAlgorithm is the core journey-planning algorithm.
type RaptorAlgorithm struct {
	routeStopIndex      RouteStopIndex
	routePath           RoutePaths
	transfers           TransfersByOrigin
	interchange         Interchange
	scanResultsFactory  *ScanResultsFactory
	queueFactory        *QueueFactory
	routeScannerFactory *RouteScannerFactory
}

// Scan performs a plan at a given time, returning the kConnections index and
// best arrivals.
func (r *RaptorAlgorithm) Scan(origins StopTimes, date DateNumber, dow DayOfWeek) (*ConnectionIndex, Arrivals) {
	routeScanner := r.routeScannerFactory.Create(date, dow)
	results := r.scanResultsFactory.Create(origins)

	// Initial marked stops = Object.keys(origins) in insertion order.
	markedStops := origins.Keys()

	for len(markedStops) > 0 {
		results.addRound()

		r.scanRoutes(results, routeScanner, markedStops)
		r.scanTransfers(results, markedStops)

		markedStops = results.getMarkedStops()
	}

	return results.finalize()
}

func (r *RaptorAlgorithm) scanRoutes(results *ScanResults, routeScanner *RouteScanner, markedStops []StopID) {
	queue := r.queueFactory.GetQueue(markedStops)

	for _, routeId := range queue.Keys() {
		stopP, _ := queue.Get(routeId)

		boardingPoint := -1
		var trip *Trip
		routePath := r.routePath[routeId]
		routePathLength := len(routePath)

		for pi := r.routeStopIndex[routeId][stopP]; pi < routePathLength; pi++ {
			stopPi := routePath[pi]
			previousArrival, paOK := results.previousArrival(stopPi)
			// JS truthiness: undefined or 0 is falsy.
			paTruthy := paOK && previousArrival != 0

			if trip != nil {
				i := r.interchange[stopPi]
				stopTime := trip.StopTimes[pi]

				if stopTime.DropOff && stopTime.ArrivalTime+i < results.bestArrival(stopPi) {
					results.setTrip(trip, boardingPoint, pi, i)
				} else if paTruthy && previousArrival < stopTime.ArrivalTime+i {
					newTrip := routeScanner.GetTrip(routeId, pi, previousArrival)
					if newTrip != nil {
						trip = newTrip
						boardingPoint = pi
					}
				}
			} else if paTruthy {
				newTrip := routeScanner.GetTrip(routeId, pi, previousArrival)
				if newTrip != nil {
					trip = newTrip
					boardingPoint = pi
				}
			}
		}
	}
}

func (r *RaptorAlgorithm) scanTransfers(results *ScanResults, markedStops []StopID) {
	for _, stopP := range markedStops {
		for _, transfer := range r.transfers[stopP] {
			stopPi := transfer.Destination
			prev, _ := results.previousArrival(stopP)
			arrival := prev + transfer.Duration + r.interchange[stopPi]

			if transfer.StartTime <= arrival && transfer.EndTime >= arrival && arrival < results.bestArrival(stopPi) {
				results.setTransfer(transfer, arrival)
			}
		}
	}
}
