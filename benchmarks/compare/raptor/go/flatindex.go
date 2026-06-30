package raptor

// FlatIndex holds the global integer-indexed arrays for the RAPTOR algorithm.
// Stop IDs and route IDs are interned to dense uint32 indices.
//
// Layout:
//   stopTimes (trip-major): for route r, trip t (0-based), stop s (0-based):
//     arrivals[route.StopTimesBase + t*route.NumStops + s]
//     departures[route.StopTimesBase + t*route.NumStops + s]
//
//   routeStops: for route r, stop p (0-based):
//     routeStops[route.RouteStopsBase + p]   -> stop index
//
//   stopRoutes inverse: for stop idx:
//     stopRoutes[stopRoutesBase[idx] .. stopRoutesBase[idx+1])  -> route indices

const InfArrival uint32 = 3_999_999_999 // sentinel: large but no overflow on add

// RouteEntry describes one route's position in the global flat slices.
type RouteEntry struct {
	StopTimesBase  uint32 // first slot in arrivals[]/departures[] for this route
	RouteStopsBase uint32 // first slot in routeStops[] for this route
	NumStops       uint32 // number of stops on the route
	NumTrips       uint32 // number of trips on the route
}

// FlatIndex is the precomputed integer-indexed representation of the timetable.
type FlatIndex struct {
	// Interning
	stopIndexOf map[StopID]uint32 // stop string -> dense index
	stopNames   []StopID          // index -> stop string

	routeIndexOf map[RouteID]uint32 // route sig -> dense index
	// (route strings not needed after build)

	// Per-route metadata
	routes []RouteEntry

	// Global trip-major stop-times arrays (indexed as above)
	arrivals   []uint32
	departures []uint32

	// routeStops[route.RouteStopsBase + p] = stop index
	routeStops []uint32

	// Inverse: which routes serve each stop, and position within route.
	// stopRoutes[stopRoutesBase[s] .. stopRoutesBase[s+1]) = route indices
	// stopRoutePos[stopRoutesBase[s] + i] = stop position within that route
	stopRoutes    []uint32
	stopRoutePos  []uint32
	stopRoutesEnd []uint32 // stopRoutesEnd[s] = end index in stopRoutes (exclusive)
	// (start = stopRoutesBase[s])
	stopRoutesBase []uint32 // stopRoutesBase[s] = start index in stopRoutes

	// Interchange by stop index (in seconds)
	interchange []uint32

	// Transfers by stop index (retains string-keyed Transfer objects for
	// journey reconstruction; FlatTransfer holds the hot-path data)
	transfersFlat [][]FlatTransfer // indexed by source stop index
	transfersFull [][]*Transfer    // same indexing, for reconstruction

	// Per-route trips (for calendar check + reconstruction)
	routeTrips [][]*Trip // routeTrips[routeIdx][tripIdx]

	// usefulStopsOrder: stop indices in insertion order (for ScanResultsFactory)
	usefulStopsOrder []uint32
}

// FlatTransfer is the hot-path representation of a transfer.
type FlatTransfer struct {
	Destination uint32
	Duration    uint32
	StartTime   uint32
	EndTime     uint32
}
