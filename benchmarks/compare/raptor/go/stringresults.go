package raptor

// StringSet is an insertion-ordered set of strings (mirrors a JS Set; equality
// in tests is membership-based).
type StringSet struct {
	order   []string
	members map[string]bool
}

// NewStringSet constructs an empty set.
func NewStringSet() *StringSet {
	return &StringSet{members: map[string]bool{}}
}

// Add inserts a value if not present.
func (s *StringSet) Add(v string) {
	if !s.members[v] {
		s.members[v] = true
		s.order = append(s.order, v)
	}
}

// Values returns the values in insertion order.
func (s *StringSet) Values() []string { return s.order }

// Has reports membership.
func (s *StringSet) Has(v string) bool { return s.members[v] }

// TransferPatternIndex maps a journey key to a set of pattern strings.
type TransferPatternIndex = *OrderedMap[*StringSet]

// StringResults stores kConnection results as origin>destination keyed pattern
// strings.
type StringResults struct {
	results     *OrderedMap[*StringSet]
	interchange Interchange
}

// NewStringResults constructs a StringResults with the given interchange index.
func NewStringResults(interchange Interchange) *StringResults {
	if interchange == nil {
		interchange = Interchange{}
	}
	return &StringResults{results: NewOrderedMap[*StringSet](), interchange: interchange}
}

// Add extracts the path from each kConnection result and stores it. Returns the
// next departure time (min over departures + 1).
func (s *StringResults) Add(kConnections *ConnectionIndex) Time {
	nextDepartureTime := MaxSafeInteger

	for _, destination := range kConnections.Stops() {
		round := kConnections.Round(destination)
		for _, k := range round.rounds {
			path, departureTime := s.getPath(kConnections, k, destination)

			if len(path) >= 1 {
				origin := path[0]
				tail := append([]StopID{}, path[1:]...)

				var journeyKey, pathString string
				if origin > destination {
					journeyKey = destination + origin
					// reverse tail
					for l, r := 0, len(tail)-1; l < r; l, r = l+1, r-1 {
						tail[l], tail[r] = tail[r], tail[l]
					}
					pathString = join(tail, ",")
				} else {
					journeyKey = origin + destination
					pathString = join(tail, ",")
				}

				set, ok := s.results.Get(journeyKey)
				if !ok {
					set = NewStringSet()
					s.results.Set(journeyKey, set)
				}
				set.Add(pathString)
				if departureTime+1 < nextDepartureTime {
					nextDepartureTime = departureTime + 1
				}
			}
		}
	}

	return nextDepartureTime
}

// Finalize returns the result index.
func (s *StringResults) Finalize() *OrderedMap[*StringSet] {
	return s.results
}

func (s *StringResults) getPath(kConnections *ConnectionIndex, k int, finalDestination StopID) (Path, Time) {
	path := Path{}
	departureTime := MaxSafeInteger

	destination := finalDestination
	for i := k; i > 0; i-- {
		connection := kConnections.Round(destination).get(i)
		var origin StopID
		if connection.IsTransfer() {
			origin = connection.Transfer.Origin
			departureTime = departureTime - connection.Transfer.Duration - s.interchange[connection.Transfer.Destination]
		} else {
			origin = connection.Trip.StopTimes[connection.StartIndex].Stop
			departureTime = connection.Trip.StopTimes[connection.StartIndex].DepartureTime
		}

		// unshift (prepend)
		path = append([]StopID{origin}, path...)
		destination = origin
	}

	return path, departureTime
}

func join(parts []string, sep string) string {
	out := ""
	for i, p := range parts {
		if i > 0 {
			out += sep
		}
		out += p
	}
	return out
}
