package raptor

import "testing"

func TestQueueFactory(t *testing.T) {
	t.Run("enqueues stops", func(t *testing.T) {
		f := NewQueueFactory(
			RoutesIndexedByStop{
				"StopA": {"RouteA", "RouteB"},
				"StopB": {"RouteB", "RouteC"},
			},
			RouteStopIndex{
				"RouteA": {"StopA": 1},
				"RouteB": {"StopA": 2, "StopB": 1},
				"RouteC": {"StopB": 1},
			},
		)

		actual := f.GetQueue([]StopID{"StopA", "StopB"})
		expected := map[RouteID]StopID{"RouteA": "StopA", "RouteB": "StopB", "RouteC": "StopB"}
		assertQueue(t, actual, expected)
	})

	t.Run("picks the earliest stop on the route", func(t *testing.T) {
		f := NewQueueFactory(
			RoutesIndexedByStop{
				"StopA": {"RouteA", "RouteB"},
				"StopB": {"RouteB", "RouteC"},
			},
			RouteStopIndex{
				"RouteA": {"StopA": 1},
				"RouteB": {"StopA": 1, "StopB": 2},
				"RouteC": {"StopB": 1},
			},
		)

		actual := f.GetQueue([]StopID{"StopB", "StopA"})
		expected := map[RouteID]StopID{"RouteA": "StopA", "RouteB": "StopA", "RouteC": "StopB"}
		assertQueue(t, actual, expected)
	})
}

func assertQueue(t *testing.T, actual *OrderedMap[StopID], expected map[RouteID]StopID) {
	t.Helper()
	if actual.Len() != len(expected) {
		t.Fatalf("queue size = %d, want %d", actual.Len(), len(expected))
	}
	for k, want := range expected {
		got, ok := actual.Get(k)
		if !ok || got != want {
			t.Errorf("queue[%q] = %q (ok=%v), want %q", k, got, ok, want)
		}
	}
}
