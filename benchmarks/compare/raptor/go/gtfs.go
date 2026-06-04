package raptor

// MaxSafeInteger is JS Number.MAX_SAFE_INTEGER (2^53 - 1). Used as the
// "infinity" arrival sentinel and as the default Transfer.endTime.
const MaxSafeInteger int64 = 9007199254740991

// StopID e.g. "NRW".
type StopID = string

// Time in seconds since midnight (may exceed 24 hours).
type Time = int64

// Duration in seconds.
type Duration = int64

// TripID is a GTFS trip_id.
type TripID = string

// ServiceID is a GTFS service_id.
type ServiceID = string

// DateNumber is a date stored as a number, e.g. 20181225.
type DateNumber = int

// DayOfWeek: Sunday = 0 .. Saturday = 6 (matching JS Date.getDay).
type DayOfWeek = int

// RouteID is the route signature string.
type RouteID = string

// StopTime is a GTFS stop time.
type StopTime struct {
	Stop          StopID
	ArrivalTime   Time
	DepartureTime Time
	PickUp        bool
	DropOff       bool
}

// Trip is a GTFS trip.
type Trip struct {
	TripID    TripID
	StopTimes []StopTime
	ServiceID ServiceID
	Service   *Service
}

// Transfer is a leg with a duration instead of departure / arrival times.
type Transfer struct {
	Origin      StopID
	Destination StopID
	Duration    Duration
	StartTime   Time
	EndTime     Time
}

// TimetableLeg is a leg with concrete stop times.
type TimetableLeg struct {
	Origin      StopID
	Destination StopID
	StopTimes   []StopTime
	Trip        *Trip
}

// AnyLeg is either a *TimetableLeg or a *Transfer.
type AnyLeg interface {
	legOrigin() StopID
	legDestination() StopID
}

func (t *Transfer) legOrigin() StopID          { return t.Origin }
func (t *Transfer) legDestination() StopID     { return t.Destination }
func (l *TimetableLeg) legOrigin() StopID      { return l.Origin }
func (l *TimetableLeg) legDestination() StopID { return l.Destination }

// Journey is a collection of legs with computed departure / arrival times.
type Journey struct {
	Legs          []AnyLeg
	DepartureTime Time
	ArrivalTime   Time
}
