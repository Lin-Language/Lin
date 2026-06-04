package raptor

import "sort"

// JourneyFilter filters a list of journeys.
type JourneyFilter interface {
	Apply(journeys []Journey) []Journey
}

// FilterCriteria returns true if b is at least as good as a in some dimension.
type FilterCriteria func(a, b Journey) bool

// EarliestArrival returns true if b arrives at or before a.
func EarliestArrival(a, b Journey) bool { return b.ArrivalTime <= a.ArrivalTime }

// LeastChanges returns true if b has the same or fewer changes than a.
func LeastChanges(a, b Journey) bool { return len(b.Legs) <= len(a.Legs) }

// MultipleCriteriaFilter sorts journeys and removes dominated ones.
type MultipleCriteriaFilter struct {
	criteria []FilterCriteria
}

// NewMultipleCriteriaFilter constructs a filter with the default criteria
// (earliestArrival + leastChanges).
func NewMultipleCriteriaFilter(criteria ...FilterCriteria) *MultipleCriteriaFilter {
	if len(criteria) == 0 {
		criteria = []FilterCriteria{EarliestArrival, LeastChanges}
	}
	return &MultipleCriteriaFilter{criteria: criteria}
}

// Apply sorts the journeys (departure asc, arrival desc tiebreak) and keeps
// each journey unless some later journey dominates it on all criteria.
func (f *MultipleCriteriaFilter) Apply(journeys []Journey) []Journey {
	sort.SliceStable(journeys, func(i, j int) bool {
		a, b := journeys[i], journeys[j]
		if a.DepartureTime != b.DepartureTime {
			return a.DepartureTime < b.DepartureTime
		}
		// arrival time descending tiebreak
		return a.ArrivalTime > b.ArrivalTime
	})

	out := []Journey{}
	for i := range journeys {
		if f.keep(journeys, i) {
			out = append(out, journeys[i])
		}
	}
	return out
}

func (f *MultipleCriteriaFilter) keep(journeys []Journey, index int) bool {
	a := journeys[index]
	for j := index + 1; j < len(journeys); j++ {
		b := journeys[j]
		dominated := true
		for _, c := range f.criteria {
			if !c(a, b) {
				dominated = false
				break
			}
		}
		if dominated {
			return false
		}
	}
	return true
}
