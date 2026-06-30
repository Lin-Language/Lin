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
	idx *FlatIndex
}

// Scan performs a plan at a given time, returning the kConnections index and
// best arrivals.
func (r *RaptorAlgorithm) Scan(origins StopTimes, date DateNumber, dow DayOfWeek) (*ConnectionIndex, Arrivals) {
	idx := r.idx
	numStops := uint32(len(idx.stopNames))

	// --- Initialise dense scan state ---
	bestArrivals := make([]uint32, numStops)
	for i := range bestArrivals {
		bestArrivals[i] = InfArrival
	}

	// round 0: origin arrivals (dense)
	prevArrivals := make([]uint32, numStops)
	for i := range prevArrivals {
		prevArrivals[i] = InfArrival
	}
	// connection index: keyed by stop string (for output compatibility)
	connections := newConnectionIndex()
	for _, stop := range idx.usefulStopsOrder {
		connections.stops.Set(idx.stopNames[stop], newRoundConnections())
	}

	// Initial marked stops from origins (insertion-ordered).
	var markedStopInts []uint32
	for _, stopName := range origins.Keys() {
		sidx, ok := idx.stopIndexOf[stopName]
		if !ok {
			continue
		}
		t, _ := origins.Get(stopName)
		if t != 0 {
			bestArrivals[sidx] = uint32(t)
			prevArrivals[sidx] = uint32(t)
		}
	}
	// Build marked stops in same order as origins.keys().
	for _, stopName := range origins.Keys() {
		sidx, ok := idx.stopIndexOf[stopName]
		if !ok {
			continue
		}
		t, ok2 := origins.Get(stopName)
		if ok2 && t != 0 {
			markedStopInts = append(markedStopInts, sidx)
		}
	}

	routeScanner := &FlatRouteScanner{
		idx:          idx,
		date:         date,
		dow:          dow,
		scanPosition: make([]int32, len(idx.routes)),
		scanPosSet:   make([]bool, len(idx.routes)),
	}
	for i := range routeScanner.scanPosition {
		routeScanner.scanPosition[i] = -1
	}

	k := 0
	for len(markedStopInts) > 0 {
		k++

		currArrivals := make([]uint32, numStops)
		for i := range currArrivals {
			currArrivals[i] = InfArrival
		}

		// Both scans read from prevArrivals (previous round) and the same initialMarked,
		// matching Node's semantics where scanRoutes and scanTransfers both receive
		// the same markedStops list.
		initialMarked := markedStopInts
		routeMarked := r.scanRoutes(idx, routeScanner, initialMarked, prevArrivals, bestArrivals, currArrivals, connections, k)
		markedStopInts = r.scanTransfers(idx, initialMarked, routeMarked, prevArrivals, bestArrivals, currArrivals, connections, k)

		prevArrivals = currArrivals
	}

	// Convert dense bestArrivals to string-keyed map (for output).
	arrivals := make(Arrivals, len(idx.usefulStopsOrder))
	for _, sidx := range idx.usefulStopsOrder {
		v := bestArrivals[sidx]
		var t Time
		if v == InfArrival {
			t = MaxSafeInteger
		} else {
			t = Time(v)
		}
		arrivals[idx.stopNames[sidx]] = t
	}

	return connections, arrivals
}

func (r *RaptorAlgorithm) scanRoutes(
	idx *FlatIndex,
	rs *FlatRouteScanner,
	markedStops []uint32,
	prevArrivals []uint32,
	bestArrivals []uint32,
	currArrivals []uint32,
	connections *ConnectionIndex,
	k int,
) []uint32 {
	// Build route queue: for each marked stop, find serving routes and pick
	// the earliest marked stop (by position) on each route.
	// queue: routeIdx -> earliest stopPosition that is marked
	// We use a slice indexed by route for O(1) update.
	numRoutes := len(idx.routes)
	queueStop := make([]uint32, numRoutes)   // stop index at the queue entry
	queuePos := make([]uint32, numRoutes)    // position within route
	queueSet := make([]bool, numRoutes)
	var queueOrder []uint32 // route indices in insertion order

	for _, stopIdx := range markedStops {
		base := idx.stopRoutesBase[stopIdx]
		end := idx.stopRoutesEnd[stopIdx]
		for i := base; i < end; i++ {
			ri := idx.stopRoutes[i]
			pos := idx.stopRoutePos[i]
			if !queueSet[ri] {
				queueSet[ri] = true
				queueStop[ri] = stopIdx
				queuePos[ri] = pos
				queueOrder = append(queueOrder, ri)
			} else if pos < queuePos[ri] {
				queueStop[ri] = stopIdx
				queuePos[ri] = pos
			}
		}
	}

	var newMarked []uint32
	markedThisRound := make([]bool, len(idx.stopNames))

	for _, ri := range queueOrder {
		re := idx.routes[ri]
		startPos := queuePos[ri]
		numStops := re.NumStops

		boardingTripIdx := int32(-1)
		boardingPos := uint32(0)

		for pi := startPos; pi < numStops; pi++ {
			stopPi := idx.routeStops[re.RouteStopsBase+pi]
			prevArr := prevArrivals[stopPi]
			paTruthy := prevArr != InfArrival && prevArr != 0

			if boardingTripIdx >= 0 {
				ic := idx.interchange[stopPi]
				arrBase := re.StopTimesBase + uint32(boardingTripIdx)*numStops
				arrTime := idx.arrivals[arrBase+pi] + ic

				// Check DropOff — use first trip's StopTimes for the flag (same for all trips on route).
				trip := idx.routeTrips[ri][boardingTripIdx]
				if trip.StopTimes[pi].DropOff && arrTime < bestArrivals[stopPi] {
					bestArrivals[stopPi] = arrTime
					currArrivals[stopPi] = arrTime
					// Record connection (uses string keys for output).
					stopName := idx.stopNames[stopPi]
					connections.Round(stopName).set(k, &Connection{
						Trip:       trip,
						StartIndex: int(boardingPos),
						EndIndex:   int(pi),
					})
					if !markedThisRound[stopPi] {
						markedThisRound[stopPi] = true
						newMarked = append(newMarked, stopPi)
					}
				} else if prevArr != InfArrival && prevArr < arrTime {
					// Try to get a better trip: person arrived before the current trip does here.
					newTripIdx := rs.GetTrip(ri, pi, prevArr)
					if newTripIdx >= 0 {
						boardingTripIdx = newTripIdx
						boardingPos = pi
					}
				}
			} else if paTruthy {
				newTripIdx := rs.GetTrip(ri, pi, prevArr)
				if newTripIdx >= 0 {
					boardingTripIdx = newTripIdx
					boardingPos = pi
				}
			}
		}
	}

	return newMarked
}

func (r *RaptorAlgorithm) scanTransfers(
	idx *FlatIndex,
	initialMarked []uint32,
	routeMarked []uint32,
	prevArrivals []uint32,
	bestArrivals []uint32,
	currArrivals []uint32,
	connections *ConnectionIndex,
	k int,
) []uint32 {
	// Scan transfers from the same initial marked stops that scanRoutes used,
	// matching Node's semantics. The result set starts with routeMarked stops
	// (already improved by routes) and we append transfer-improved stops.

	markedThisRound := make([]bool, len(idx.stopNames))
	for _, s := range routeMarked {
		markedThisRound[s] = true
	}

	result := routeMarked
	for _, stopP := range initialMarked {
		prev := prevArrivals[stopP]
		if prev == InfArrival {
			continue
		}

		for i, ft := range idx.transfersFlat[stopP] {
			if ft.Destination == ^uint32(0) {
				continue
			}
			stopPi := ft.Destination
			arrival := prev + ft.Duration + idx.interchange[stopPi]
			if ft.StartTime <= arrival && ft.EndTime >= arrival && arrival < bestArrivals[stopPi] {
				bestArrivals[stopPi] = arrival
				currArrivals[stopPi] = arrival
				transfer := idx.transfersFull[stopP][i]
				stopName := idx.stopNames[stopPi]
				connections.Round(stopName).set(k, &Connection{Transfer: transfer})
				if !markedThisRound[stopPi] {
					markedThisRound[stopPi] = true
					result = append(result, stopPi)
				}
			}
		}
	}

	return result
}
