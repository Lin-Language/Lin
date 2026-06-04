package raptor

import "testing"

// buildSyntheticKConnections mirrors the spec mergePath helper: for each
// adjacent pair (origin, destination) in path it sets
// kConnections[destination][i] = [{stopTimes:[{stop:origin}]}, 0, 1].
func buildSyntheticKConnections(path []StopID) *ConnectionIndex {
	ci := newConnectionIndex()
	for i := 1; i < len(path); i++ {
		origin := path[i-1]
		destination := path[i]
		// kConnections[destination] = {} (overwrite each time, like the helper)
		round := newRoundConnections()
		ci.stops.Set(destination, round)
		trip := &Trip{StopTimes: []StopTime{{Stop: origin}}}
		round.set(i, &Connection{Trip: trip, StartIndex: 0, EndIndex: 1})
	}
	return ci
}

func TestGraphResults(t *testing.T) {
	t.Run("Merges a path into an empty tree", func(t *testing.T) {
		tree := NewGraphResults()
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B", "C"}))

		expected := map[StopID][]Path{
			"A": {{"A"}},
			"B": {{"B", "A"}},
			"C": {{"C", "B", "A"}},
		}
		assertGraph(t, tree.Finalize(), expected)
	})

	t.Run("Merges duplicate paths", func(t *testing.T) {
		tree := NewGraphResults()
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B", "C"}))
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B"}))

		expected := map[StopID][]Path{
			"A": {{"A"}},
			"B": {{"B", "A"}},
			"C": {{"C", "B", "A"}},
		}
		assertGraph(t, tree.Finalize(), expected)
	})

	t.Run("Appends to existing paths", func(t *testing.T) {
		tree := NewGraphResults()
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B"}))
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B", "C"}))

		expected := map[StopID][]Path{
			"A": {{"A"}},
			"B": {{"B", "A"}},
			"C": {{"C", "B", "A"}},
		}
		assertGraph(t, tree.Finalize(), expected)
	})

	t.Run("Appends different paths", func(t *testing.T) {
		tree := NewGraphResults()
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B", "C", "D"}))
		tree.Add(buildSyntheticKConnections([]StopID{"A", "B", "D"}))

		expected := map[StopID][]Path{
			"A": {{"A"}},
			"B": {{"B", "A"}},
			"C": {{"C", "B", "A"}},
			"D": {{"D", "C", "B", "A"}, {"D", "B", "A"}},
		}
		assertGraph(t, tree.Finalize(), expected)
	})
}

// assertGraph compares the finalized graph against an expected map of
// label -> list of parent-chain label sequences (the chain of each node,
// starting at the node's own label).
func assertGraph(t *testing.T, got *OrderedMap[[]*TreeNode], expected map[StopID][]Path) {
	t.Helper()
	if got.Len() != len(expected) {
		t.Fatalf("graph has %d labels, want %d (keys=%v)", got.Len(), len(expected), got.Keys())
	}
	for label, wantChains := range expected {
		nodes, ok := got.Get(label)
		if !ok {
			t.Fatalf("missing label %q", label)
		}
		if len(nodes) != len(wantChains) {
			t.Fatalf("label %q has %d nodes, want %d", label, len(nodes), len(wantChains))
		}
		for i, node := range nodes {
			chain := nodeChain(node)
			if !equalStrings(chain, wantChains[i]) {
				t.Fatalf("label %q node %d chain = %v, want %v", label, i, chain, wantChains[i])
			}
		}
	}
}

func nodeChain(n *TreeNode) []StopID {
	out := []StopID{}
	for n != nil {
		out = append(out, n.Label)
		n = n.Parent
	}
	return out
}

func equalStrings(a, b []string) bool {
	if len(a) != len(b) {
		return false
	}
	for i := range a {
		if a[i] != b[i] {
			return false
		}
	}
	return true
}
