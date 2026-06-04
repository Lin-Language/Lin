package raptor

import "sort"

// ResultsFactory creates journeys from the kConnections index.
type ResultsFactory interface {
	GetResults(kConnections *ConnectionIndex, destination StopID) []Journey
}

// JourneyFactory extracts journeys from the kConnections index.
type JourneyFactory struct{}

// NewJourneyFactory constructs a JourneyFactory.
func NewJourneyFactory() *JourneyFactory { return &JourneyFactory{} }

// GetResults takes the best result of each round for the destination and turns
// each into a journey. Rounds are iterated in numeric ascending order.
func (f *JourneyFactory) GetResults(kConnections *ConnectionIndex, destination StopID) []Journey {
	results := []Journey{}

	round := kConnections.Round(destination)
	if round == nil {
		return results
	}

	rounds := make([]int, len(round.rounds))
	copy(rounds, round.rounds)
	sort.Ints(rounds)

	for _, k := range rounds {
		legs := f.getJourneyLegs(kConnections, k, destination)
		results = append(results, Journey{
			Legs:          legs,
			DepartureTime: getDepartureTime(legs),
			ArrivalTime:   getArrivalTime(legs),
		})
	}

	return results
}

func (f *JourneyFactory) getJourneyLegs(kConnections *ConnectionIndex, k int, finalDestination StopID) []AnyLeg {
	legs := []AnyLeg{}

	destination := finalDestination
	for i := k; i > 0; i-- {
		connection := kConnections.Round(destination).get(i)

		if connection.IsTransfer() {
			legs = append(legs, connection.Transfer)
			destination = connection.Transfer.Origin
		} else {
			trip := connection.Trip
			stopTimes := trip.StopTimes[connection.StartIndex : connection.EndIndex+1]
			origin := stopTimes[0].Stop
			legs = append(legs, &TimetableLeg{
				StopTimes:   stopTimes,
				Origin:      origin,
				Destination: destination,
				Trip:        trip,
			})
			destination = origin
		}
	}

	// reverse
	for l, r := 0, len(legs)-1; l < r; l, r = l+1, r-1 {
		legs[l], legs[r] = legs[r], legs[l]
	}
	return legs
}

func getDepartureTime(legs []AnyLeg) Time {
	var transferDuration Time
	for _, leg := range legs {
		if tl, ok := leg.(*TimetableLeg); ok {
			return tl.StopTimes[0].DepartureTime - transferDuration
		}
		transferDuration += leg.(*Transfer).Duration
	}
	return 0
}

func getArrivalTime(legs []AnyLeg) Time {
	var transferDuration Time
	for i := len(legs) - 1; i >= 0; i-- {
		leg := legs[i]
		if tl, ok := leg.(*TimetableLeg); ok {
			return tl.StopTimes[len(tl.StopTimes)-1].ArrivalTime + transferDuration
		}
		transferDuration += leg.(*Transfer).Duration
	}
	return 0
}
