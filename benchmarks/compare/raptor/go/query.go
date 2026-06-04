package raptor

// GroupStationDepartAfterQuery searches for journeys between a set of origin
// and destination stops, stitching together multiple days if needed.
type GroupStationDepartAfterQuery struct {
	raptor         *RaptorAlgorithm
	resultsFactory ResultsFactory
	maxSearchDays  int
	filters        []JourneyFilter
}

// NewGroupStationDepartAfterQuery constructs the query.
func NewGroupStationDepartAfterQuery(raptor *RaptorAlgorithm, resultsFactory ResultsFactory, maxSearchDays int, filters []JourneyFilter) *GroupStationDepartAfterQuery {
	if maxSearchDays <= 0 {
		maxSearchDays = 3
	}
	return &GroupStationDepartAfterQuery{
		raptor:         raptor,
		resultsFactory: resultsFactory,
		maxSearchDays:  maxSearchDays,
		filters:        filters,
	}
}

// Plan plans a journey between the origin and destination sets.
func (q *GroupStationDepartAfterQuery) Plan(origins, destinations []StopID, date SearchDate, time Time) []Journey {
	originTimes := NewOrderedMap[Time]()
	for _, o := range origins {
		originTimes.Set(o, time)
	}

	results := q.getJourneys(originTimes, destinations, date)

	for _, filter := range q.filters {
		results = filter.Apply(results)
	}
	return results
}

func (q *GroupStationDepartAfterQuery) getJourneys(origins StopTimes, destinations []StopID, startDate SearchDate) []Journey {
	connectionIndexes := []*ConnectionIndex{}

	for i := 0; i < q.maxSearchDays; i++ {
		date := startDate.GetDateNumber()
		dow := startDate.DayOfWeek()
		kConnections, bestArrivals := q.raptor.Scan(origins, date, dow)
		results := q.getJourneysFromConnections(kConnections, connectionIndexes, destinations)

		if len(results) > 0 {
			return results
		}

		origins = q.getFoundStations(kConnections, bestArrivals)
		startDate = startDate.AddDay()
		connectionIndexes = append(connectionIndexes, kConnections)
	}

	return []Journey{}
}

func (q *GroupStationDepartAfterQuery) getFoundStations(kConnections *ConnectionIndex, bestArrivals Arrivals) StopTimes {
	out := NewOrderedMap[Time]()
	for _, stop := range kConnections.Stops() {
		if kConnections.HasResults(stop) {
			v := bestArrivals[stop] - 86400
			if v < 1 {
				v = 1
			}
			out.Set(stop, v)
		}
	}
	return out
}

func (q *GroupStationDepartAfterQuery) getJourneysFromConnections(kConnections *ConnectionIndex, prevConnections []*ConnectionIndex, destinations []StopID) []Journey {
	initialResults := []Journey{}
	for _, d := range destinations {
		if kConnections.HasResults(d) {
			initialResults = append(initialResults, q.resultsFactory.GetResults(kConnections, d)...)
		}
	}

	// reverse the previous connections, then work back prepending journeys
	results := initialResults
	for i := len(prevConnections) - 1; i >= 0; i-- {
		results = q.completeJourneys(results, prevConnections[i])
	}
	return results
}

func (q *GroupStationDepartAfterQuery) completeJourneys(results []Journey, kConnections *ConnectionIndex) []Journey {
	out := []Journey{}
	for _, journeyB := range results {
		origin := journeyB.Legs[0].legOrigin()
		for _, journeyA := range q.resultsFactory.GetResults(kConnections, origin) {
			out = append(out, mergeJourneys(journeyA, journeyB))
		}
	}
	return out
}

func mergeJourneys(journeyA, journeyB Journey) Journey {
	legs := make([]AnyLeg, 0, len(journeyA.Legs)+len(journeyB.Legs))
	legs = append(legs, journeyA.Legs...)
	legs = append(legs, journeyB.Legs...)
	return Journey{
		Legs:          legs,
		DepartureTime: journeyA.DepartureTime,
		ArrivalTime:   journeyB.ArrivalTime + 86400,
	}
}

// DepartAfterQuery is a single-origin/destination wrapper.
type DepartAfterQuery struct {
	groupQuery *GroupStationDepartAfterQuery
}

// NewDepartAfterQuery constructs the query with the default 3-day search.
func NewDepartAfterQuery(raptor *RaptorAlgorithm, resultsFactory ResultsFactory, maxSearchDays int) *DepartAfterQuery {
	if maxSearchDays <= 0 {
		maxSearchDays = 3
	}
	return &DepartAfterQuery{
		groupQuery: NewGroupStationDepartAfterQuery(raptor, resultsFactory, maxSearchDays, nil),
	}
}

// Plan plans a journey between a single origin and destination. No filters.
func (q *DepartAfterQuery) Plan(origin, destination StopID, date SearchDate, time Time) []Journey {
	return q.groupQuery.Plan([]StopID{origin}, []StopID{destination}, date, time)
}

// RangeQuery performs a profile (full-day) query.
type RangeQuery struct {
	groupQuery *GroupStationDepartAfterQuery
	filters    []JourneyFilter
}

const oneDay Time = 24 * 60 * 60

// NewRangeQuery constructs a profile query.
func NewRangeQuery(raptor *RaptorAlgorithm, resultsFactory ResultsFactory, maxSearchDays int, filters []JourneyFilter) *RangeQuery {
	if maxSearchDays <= 0 {
		maxSearchDays = 3
	}
	return &RangeQuery{
		groupQuery: NewGroupStationDepartAfterQuery(raptor, resultsFactory, maxSearchDays, nil),
		filters:    filters,
	}
}

// Plan performs a query starting at `time` (default 1) repeating one minute
// after the earliest departure of each result set, up to endTime (default
// ONE_DAY).
func (q *RangeQuery) Plan(origin, destination StopID, date SearchDate, time, endTime Time) []Journey {
	if time == 0 {
		time = 1
	}
	if endTime == 0 {
		endTime = oneDay
	}

	results := []Journey{}

	for time < endTime {
		newResults := q.groupQuery.Plan([]StopID{origin}, []StopID{destination}, date, time)
		results = append(results, newResults...)

		if len(newResults) == 0 {
			break
		}

		min := newResults[0].DepartureTime
		for _, j := range newResults[1:] {
			if j.DepartureTime < min {
				min = j.DepartureTime
			}
		}
		time = min + 1
	}

	for _, filter := range q.filters {
		results = filter.Apply(results)
	}
	return results
}
