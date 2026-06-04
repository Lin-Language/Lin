package raptor

// Connection is one round's connection to a stop: either a timetable
// connection [trip, startIndex, endIndex] or a transfer.
type Connection struct {
	// Transfer is non-nil for a transfer connection.
	Transfer *Transfer

	// The following are valid only when Transfer == nil (timetable leg).
	Trip       *Trip
	StartIndex int
	EndIndex   int
}

// IsTransfer reports whether this is a transfer connection (mirrors isTransfer
// which checks for an `origin` field).
func (c *Connection) IsTransfer() bool {
	return c.Transfer != nil
}

// Arrivals maps a stop to its best arrival time.
type Arrivals = map[StopID]Time

// RoundConnections maps a round number (k) to a Connection, preserving the
// rounds in numeric ascending order via sorted iteration in JourneyFactory.
type RoundConnections struct {
	byRound map[int]*Connection
	rounds  []int // insertion order; rounds are added strictly increasing here
}

func newRoundConnections() *RoundConnections {
	return &RoundConnections{byRound: map[int]*Connection{}}
}

func (r *RoundConnections) set(k int, c *Connection) {
	if _, ok := r.byRound[k]; !ok {
		r.rounds = append(r.rounds, k)
	}
	r.byRound[k] = c
}

func (r *RoundConnections) get(k int) *Connection { return r.byRound[k] }
func (r *RoundConnections) len() int              { return len(r.rounds) }

// ConnectionIndex maps a stop to its per-round connections.
//
// Keys (stop IDs) preserve insertion order to match JS object iteration in
// getFoundStations / GraphResults.getPaths / StringResults.add.
type ConnectionIndex struct {
	stops *OrderedMap[*RoundConnections]
}

func newConnectionIndex() *ConnectionIndex {
	return &ConnectionIndex{stops: NewOrderedMap[*RoundConnections]()}
}

// Stops returns stop IDs in insertion order.
func (ci *ConnectionIndex) Stops() []StopID { return ci.stops.Keys() }

// Round returns the per-round connections for a stop (may be empty).
func (ci *ConnectionIndex) Round(stop StopID) *RoundConnections {
	r, _ := ci.stops.Get(stop)
	return r
}

// HasResults reports whether a stop has at least one connection.
func (ci *ConnectionIndex) HasResults(stop StopID) bool {
	r, ok := ci.stops.Get(stop)
	return ok && r.len() > 0
}

// ScanResults accumulates per-round arrivals and connections during a scan.
type ScanResults struct {
	k            int
	bestArrivals Arrivals
	// kArrivals indexed by round; index 0 holds the origin arrivals. Each round
	// uses an OrderedMap so getMarkedStops returns insertion order.
	kArrivals   []*OrderedMap[Time]
	connections *ConnectionIndex
}

func (r *ScanResults) addRound() {
	r.k++
	r.kArrivals = append(r.kArrivals, NewOrderedMap[Time]())
}

func (r *ScanResults) previousArrival(stop StopID) (Time, bool) {
	return r.kArrivals[r.k-1].Get(stop)
}

func (r *ScanResults) setTrip(trip *Trip, startIndex, endIndex int, interchange Time) {
	time := trip.StopTimes[endIndex].ArrivalTime + interchange
	stop := trip.StopTimes[endIndex].Stop

	r.kArrivals[r.k].Set(stop, time)
	r.bestArrivals[stop] = time
	r.connections.Round(stop).set(r.k, &Connection{Trip: trip, StartIndex: startIndex, EndIndex: endIndex})
}

func (r *ScanResults) setTransfer(transfer *Transfer, time Time) {
	stop := transfer.Destination

	r.kArrivals[r.k].Set(stop, time)
	r.bestArrivals[stop] = time
	r.connections.Round(stop).set(r.k, &Connection{Transfer: transfer})
}

func (r *ScanResults) bestArrival(stop StopID) Time {
	return r.bestArrivals[stop]
}

func (r *ScanResults) getMarkedStops() []StopID {
	return r.kArrivals[r.k].Keys()
}

func (r *ScanResults) finalize() (*ConnectionIndex, Arrivals) {
	return r.connections, r.bestArrivals
}

// ScanResultsFactory builds a fresh ScanResults for a scan over a fixed set of
// stops.
type ScanResultsFactory struct {
	stops []StopID
}

// NewScanResultsFactory constructs the factory over the given stop universe.
func NewScanResultsFactory(stops []StopID) *ScanResultsFactory {
	return &ScanResultsFactory{stops: stops}
}

// Create initialises arrivals to the origin departure times (or +infinity) and
// an empty per-stop connection index.
func (f *ScanResultsFactory) Create(origins StopTimes) *ScanResults {
	bestArrivals := make(Arrivals, len(f.stops))
	round0 := NewOrderedMap[Time]()
	connections := newConnectionIndex()

	for _, stop := range f.stops {
		v := MaxSafeInteger
		if t, ok := origins.Get(stop); ok && t != 0 {
			v = t
		}
		bestArrivals[stop] = v
		round0.Set(stop, v)
		connections.stops.Set(stop, newRoundConnections())
	}

	return &ScanResults{
		bestArrivals: bestArrivals,
		kArrivals:    []*OrderedMap[Time]{round0},
		connections:  connections,
	}
}
