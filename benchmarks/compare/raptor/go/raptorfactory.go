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

	// Stable sort by first departure time.
	sorted := make([]*Trip, len(trips))
	copy(sorted, trips)
	sort.SliceStable(sorted, func(i, j int) bool {
		return sorted[i].StopTimes[0].DepartureTime < sorted[j].StopTimes[0].DepartureTime
	})

	// --- Pass 0: intern stops and group trips by route signature ---

	stopIndexOf := map[StopID]uint32{}
	var stopNames []StopID
	internStop := func(s StopID) uint32 {
		if idx, ok := stopIndexOf[s]; ok {
			return idx
		}
		idx := uint32(len(stopNames))
		stopIndexOf[s] = idx
		stopNames = append(stopNames, s)
		return idx
	}

	routeIndexOf := map[RouteID]uint32{}
	type routeBuild struct {
		sig   RouteID
		path  []StopID // original stop strings for reconstruction
		trips []*Trip
	}
	var routeList []*routeBuild
	routeMap := map[RouteID]*routeBuild{}

	// Insertion-ordered useful stops (mirrors original usefulTransfersOrder).
	usefulStopSeen := map[uint32]bool{}
	var usefulStopsOrder []uint32

	trackStop := func(sidx uint32) {
		if !usefulStopSeen[sidx] {
			usefulStopSeen[sidx] = true
			usefulStopsOrder = append(usefulStopsOrder, sidx)
		}
	}

	// Track overtaking: tripsByRoute for the overtake check (temporary map).
	tripsByRouteSig := map[RouteID][]*Trip{}

	for _, trip := range sorted {
		// Intern all stops on this trip.
		for _, s := range trip.StopTimes {
			internStop(s.Stop)
		}

		// Build route signature.
		sig := routeSig(trip)
		routeId := sig

		// Overtaking check (same as original).
		arrivalTimeA := trip.StopTimes[len(trip.StopTimes)-1].ArrivalTime
		for _, t := range tripsByRouteSig[routeId] {
			arrivalTimeB := t.StopTimes[len(t.StopTimes)-1].ArrivalTime
			if arrivalTimeA < arrivalTimeB {
				routeId = sig + overtakingRouteSuffix
				break
			}
		}
		tripsByRouteSig[sig] = append(tripsByRouteSig[sig], trip)

		rb := routeMap[routeId]
		if rb == nil {
			rb = &routeBuild{sig: routeId}
			// Build path (stop strings for this route).
			for _, s := range trip.StopTimes {
				rb.path = append(rb.path, s.Stop)
			}
			routeMap[routeId] = rb
			routeList = append(routeList, rb)
			routeIndexOf[routeId] = uint32(len(routeList) - 1)

			// Register useful stops and interchange (same traversal order as original).
			for i := len(rb.path) - 1; i >= 0; i-- {
				sidx := stopIndexOf[rb.path[i]]
				trackStop(sidx)
				if _, ok := interchange[rb.path[i]]; !ok {
					interchange[rb.path[i]] = 0
				}
			}
		}
		rb.trips = append(rb.trips, trip)
	}

	numStops := uint32(len(stopNames))
	numRoutes := uint32(len(routeList))

	// --- Pass 1: size the global arrays and set base offsets ---

	routes := make([]RouteEntry, numRoutes)
	var totalStopTimes uint32
	var totalRouteStops uint32
	for ri, rb := range routeList {
		ns := uint32(len(rb.path))
		nt := uint32(len(rb.trips))
		routes[ri] = RouteEntry{
			StopTimesBase:  totalStopTimes,
			RouteStopsBase: totalRouteStops,
			NumStops:       ns,
			NumTrips:       nt,
		}
		totalStopTimes += ns * nt
		totalRouteStops += ns
	}

	// --- Pass 2: fill global flat arrays ---

	arrivals := make([]uint32, totalStopTimes)
	departures := make([]uint32, totalStopTimes)
	routeStops := make([]uint32, totalRouteStops)

	// routeStops: each route's stop sequence.
	for ri, rb := range routeList {
		re := routes[ri]
		for si, sname := range rb.path {
			routeStops[re.RouteStopsBase+uint32(si)] = stopIndexOf[sname]
		}
	}

	// arrivals/departures: trip-major layout.
	for ri, rb := range routeList {
		re := routes[ri]
		for ti, trip := range rb.trips {
			base := re.StopTimesBase + uint32(ti)*re.NumStops
			for si, st := range trip.StopTimes {
				arrivals[base+uint32(si)] = uint32(st.ArrivalTime)
				departures[base+uint32(si)] = uint32(st.DepartureTime)
			}
		}
	}

	// --- Build stopRoutes inverse index ---
	// Count routes per stop first.
	stopRouteCount := make([]uint32, numStops)
	// Also track pickUp flag per (route, stop position) for the inverse index.
	// We need to only add a route to a stop's list if PickUp is true for that stop.
	// Re-scan routeList for that.
	for _, rb := range routeList {
		for si, sname := range rb.path {
			sidx := stopIndexOf[sname]
			// Only add if pickup is allowed at this stop.
			if rb.trips[0].StopTimes[si].PickUp {
				stopRouteCount[sidx]++
			}
		}
	}

	stopRoutesBase := make([]uint32, numStops+1)
	for s := uint32(0); s < numStops; s++ {
		stopRoutesBase[s+1] = stopRoutesBase[s] + stopRouteCount[s]
	}
	totalInverse := stopRoutesBase[numStops]

	stopRoutes := make([]uint32, totalInverse)
	stopRoutePos := make([]uint32, totalInverse) // position within route
	stopRoutesEnd := make([]uint32, numStops)
	copy(stopRoutesEnd, stopRoutesBase[:numStops]) // use as cursor

	for ri, rb := range routeList {
		for si, sname := range rb.path {
			sidx := stopIndexOf[sname]
			if rb.trips[0].StopTimes[si].PickUp {
				cursor := stopRoutesEnd[sidx]
				stopRoutes[cursor] = uint32(ri)
				stopRoutePos[cursor] = uint32(si)
				stopRoutesEnd[sidx]++
			}
		}
	}

	// --- Build dense interchange slice ---
	interchangeFlat := make([]uint32, numStops)
	for sname, ic := range interchange {
		if sidx, ok := stopIndexOf[sname]; ok {
			interchangeFlat[sidx] = uint32(ic)
		}
	}

	// --- Build flat transfers indexed by stop index ---
	transfersFlat := make([][]FlatTransfer, numStops)
	transfersFull := make([][]*Transfer, numStops)
	for sname, tlist := range transfers {
		sidx, ok := stopIndexOf[sname]
		if !ok {
			continue
		}
		flat := make([]FlatTransfer, len(tlist))
		for i, tr := range tlist {
			dst, dok := stopIndexOf[tr.Destination]
			if !dok {
				// destination not in any route; skip
				flat[i] = FlatTransfer{Destination: ^uint32(0)} // sentinel
				continue
			}
			endTime := uint32(tr.EndTime)
			if tr.EndTime > int64(InfArrival) {
				endTime = InfArrival
			}
			flat[i] = FlatTransfer{
				Destination: dst,
				Duration:    uint32(tr.Duration),
				StartTime:   uint32(tr.StartTime),
				EndTime:     endTime,
			}
		}
		transfersFlat[sidx] = flat
		transfersFull[sidx] = tlist
	}

	// --- Build routeTrips ---
	routeTrips := make([][]*Trip, numRoutes)
	for ri, rb := range routeList {
		routeTrips[ri] = rb.trips
	}

	idx := &FlatIndex{
		stopIndexOf:    stopIndexOf,
		stopNames:      stopNames,
		routeIndexOf:   routeIndexOf,
		routes:         routes,
		arrivals:       arrivals,
		departures:     departures,
		routeStops:     routeStops,
		stopRoutes:     stopRoutes,
		stopRoutePos:   stopRoutePos,
		stopRoutesBase: stopRoutesBase,
		stopRoutesEnd:  stopRoutesEnd,
		interchange:    interchangeFlat,
		transfersFlat:  transfersFlat,
		transfersFull:  transfersFull,
		routeTrips:     routeTrips,
		usefulStopsOrder: usefulStopsOrder,
	}

	return &RaptorAlgorithm{idx: idx}
}

// routeSig builds the route signature string from a trip.
func routeSig(trip *Trip) RouteID {
	routeId := ""
	for i, s := range trip.StopTimes {
		if i > 0 {
			routeId += ","
		}
		routeId += s.Stop + boolDigit(s.PickUp) + boolDigit(s.DropOff)
	}
	return routeId
}

func boolDigit(b bool) string {
	if b {
		return "1"
	}
	return "0"
}
