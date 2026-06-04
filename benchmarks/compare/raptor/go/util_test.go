package raptor

import (
	"fmt"
	"reflect"
	"strings"
	"testing"
)

// allDays is every day of the week enabled.
var allDays = map[DayOfWeek]bool{0: true, 1: true, 2: true, 3: true, 4: true, 5: true, 6: true}

// services mirrors the reference's two test services.
var services = map[string]*Service{
	"1": NewService(20180101, 20991231, allDays, map[DateNumber]bool{}),
	"2": NewService(20190101, 20991231, allDays, map[DateNumber]bool{}),
}

var tripIDCounter int

// t builds a trip with serviceId "1", mirroring the reference helper.
func tripOf(stopTimes ...StopTime) *Trip {
	id := fmt.Sprintf("trip%d", tripIDCounter)
	tripIDCounter++
	return &Trip{
		TripID:    id,
		StopTimes: stopTimes,
		ServiceID: "1",
		Service:   services["1"],
	}
}

// st builds a stop time. arrivalTime = arr ?? dep, departureTime = dep ?? arr,
// dropOff = arr != nil, pickUp = dep != nil. nil is represented by passing a
// negative sentinel via the *Time helpers below.
func st(stop StopID, arr, dep *Time) StopTime {
	var arrival, departure Time
	if arr != nil {
		arrival = *arr
	} else {
		arrival = *dep
	}
	if dep != nil {
		departure = *dep
	} else {
		departure = *arr
	}
	return StopTime{
		Stop:          stop,
		ArrivalTime:   arrival,
		DepartureTime: departure,
		DropOff:       arr != nil,
		PickUp:        dep != nil,
	}
}

// p is a tiny helper to take the address of a Time literal (so st can
// distinguish null from 0, exactly as the reference distinguishes null from a
// number).
func p(v Time) *Time { return &v }

// tf builds a transfer with startTime=0, endTime=MaxSafeInteger.
func tf(origin, destination StopID, duration Time) *Transfer {
	return &Transfer{Origin: origin, Destination: destination, Duration: duration, StartTime: 0, EndTime: MaxSafeInteger}
}

// legInput is either a []StopTime (timetable leg) or a *Transfer.
type legInput interface{}

// j builds a Journey from legs, computing departure / arrival times like the
// reference helper.
func j(legs ...legInput) Journey {
	out := Journey{
		DepartureTime: jDepartureTime(legs),
		ArrivalTime:   jArrivalTime(legs),
	}
	for _, leg := range legs {
		switch v := leg.(type) {
		case *Transfer:
			out.Legs = append(out.Legs, v)
		case []StopTime:
			out.Legs = append(out.Legs, &TimetableLeg{
				StopTimes:   v,
				Origin:      v[0].Stop,
				Destination: v[len(v)-1].Stop,
				Trip:        defaultTrip,
			})
		default:
			panic("invalid leg input")
		}
	}
	return out
}

func jDepartureTime(legs []legInput) Time {
	var transferDuration Time
	for _, leg := range legs {
		switch v := leg.(type) {
		case *Transfer:
			transferDuration += v.Duration
		case []StopTime:
			return v[0].DepartureTime - transferDuration
		}
	}
	return 0
}

func jArrivalTime(legs []legInput) Time {
	var transferDuration Time
	for i := len(legs) - 1; i >= 0; i-- {
		switch v := legs[i].(type) {
		case *Transfer:
			transferDuration += v.Duration
		case []StopTime:
			return v[len(v)-1].ArrivalTime + transferDuration
		}
	}
	return 0
}

// defaultTrip is the fixed trip used to normalise journey equality.
var defaultTrip = &Trip{TripID: "1", ServiceID: "1", StopTimes: []StopTime{}, Service: services["1"]}

// setDefaultTrip overwrites every timetable leg's trip with the default so that
// journey equality ignores trip identity.
func setDefaultTrip(results []Journey) {
	for _, journey := range results {
		for _, leg := range journey.Legs {
			if tl, ok := leg.(*TimetableLeg); ok {
				tl.Trip = defaultTrip
			}
		}
	}
}

// journeysEqual deep-compares two journey slices, distinguishing leg kinds and
// ignoring trip identity on timetable legs.
func journeysEqual(a, b []Journey) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if !journeyEqual(a[i], b[i]) {
			return false
		}
	}
	return true
}

func journeyEqual(a, b Journey) bool {
	if a.DepartureTime != b.DepartureTime || a.ArrivalTime != b.ArrivalTime {
		return false
	}
	if len(a.Legs) != len(b.Legs) {
		return false
	}
	for i := range a.Legs {
		if !legEqual(a.Legs[i], b.Legs[i]) {
			return false
		}
	}
	return true
}

func legsEqual(a, b []AnyLeg) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if !legEqual(a[i], b[i]) {
			return false
		}
	}
	return true
}

// assertJourneys fails the test (with a readable diff) if got != want.
func assertJourneys(t *testing.T, got, want []Journey) {
	t.Helper()
	if !journeysEqual(got, want) {
		t.Fatalf("journeys mismatch:\n got (%d):\n%s\nwant (%d):\n%s",
			len(got), journeysString(got), len(want), journeysString(want))
	}
}

func journeysString(js []Journey) string {
	var b strings.Builder
	for i, j := range js {
		fmt.Fprintf(&b, "  [%d] %s\n", i, journeyString(j))
	}
	return b.String()
}

func journeyString(j Journey) string {
	var b strings.Builder
	fmt.Fprintf(&b, "dep=%d arr=%d legs=[", j.DepartureTime, j.ArrivalTime)
	for i, leg := range j.Legs {
		if i > 0 {
			b.WriteString(" | ")
		}
		switch v := leg.(type) {
		case *Transfer:
			fmt.Fprintf(&b, "transfer %s->%s dur=%d", v.Origin, v.Destination, v.Duration)
		case *TimetableLeg:
			fmt.Fprintf(&b, "tt %s->%s ", v.Origin, v.Destination)
			for _, s := range v.StopTimes {
				fmt.Fprintf(&b, "%s(%d/%d) ", s.Stop, s.ArrivalTime, s.DepartureTime)
			}
		}
	}
	b.WriteString("]")
	return b.String()
}

func legEqual(a, b AnyLeg) bool {
	at, aIsT := a.(*Transfer)
	bt, bIsT := b.(*Transfer)
	if aIsT != bIsT {
		return false
	}
	if aIsT {
		return *at == *bt
	}
	al := a.(*TimetableLeg)
	bl := b.(*TimetableLeg)
	// ignore Trip identity
	return al.Origin == bl.Origin && al.Destination == bl.Destination &&
		reflect.DeepEqual(al.StopTimes, bl.StopTimes)
}
