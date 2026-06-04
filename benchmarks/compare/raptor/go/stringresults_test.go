package raptor

import (
	"sort"
	"testing"
)

func TestStringResults(t *testing.T) {
	t.Run("Merges duplicate paths", func(t *testing.T) {
		tree := NewStringResults(nil)
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B", "C", "D"}))
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B", "C"}))

		expected := map[string][]string{
			"AB": {""},
			"AC": {"B"},
			"AD": {"B,C"},
		}
		assertStringResults(t, tree.Finalize(), expected)
	})

	t.Run("Orders results", func(t *testing.T) {
		tree := NewStringResults(nil)
		tree.Add(buildSyntheticKConnections([]StopID{"C", "B", "A"}))
		tree.Add(buildSyntheticKConnections([]StopID{"C", "D", "B"}))
		tree.Add(buildSyntheticKConnections([]StopID{"C", "B", "E"}))

		expected := map[string][]string{
			"AC": {"B"},
			"BC": {"", "D"},
			"CE": {"B"},
			"CD": {""},
		}
		assertStringResults(t, tree.Finalize(), expected)
	})

	t.Run("Adds different paths", func(t *testing.T) {
		tree := NewStringResults(nil)
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B", "C", "D"}))
		tree.Add(buildSyntheticKConnections([]StopID{"A", "C", "B", "D"}))

		expected := map[string][]string{
			"AC": {"", "B"},
			"AB": {"", "C"},
			"AD": {"B,C", "C,B"},
		}
		assertStringResults(t, tree.Finalize(), expected)
	})
}

// assertStringResults compares the result index by key membership and set
// membership (order-independent, as JS Set / object toEqual).
func assertStringResults(t *testing.T, got *OrderedMap[*StringSet], expected map[string][]string) {
	t.Helper()
	if got.Len() != len(expected) {
		t.Fatalf("result has %d keys, want %d (keys=%v)", got.Len(), len(expected), got.Keys())
	}
	for key, wantVals := range expected {
		set, ok := got.Get(key)
		if !ok {
			t.Fatalf("missing key %q", key)
		}
		gotVals := append([]string{}, set.Values()...)
		want := append([]string{}, wantVals...)
		sort.Strings(gotVals)
		sort.Strings(want)
		if !equalStrings(gotVals, want) {
			t.Fatalf("key %q = %v, want %v", key, set.Values(), wantVals)
		}
	}
}
