package raptor

// Path is a list of stops representing a journey's path.
type Path = []StopID

// TreeNode is a graph node with a pointer to its parent.
type TreeNode struct {
	Label  StopID
	Parent *TreeNode
}

// TransferPatternGraph maps a label to its tree nodes (insertion-ordered).
type TransferPatternGraph = *OrderedMap[[]*TreeNode]

// GraphResults stores transfer patterns as a DAG.
type GraphResults struct {
	results *OrderedMap[[]*TreeNode]
}

// NewGraphResults constructs an empty GraphResults.
func NewGraphResults() *GraphResults {
	return &GraphResults{results: NewOrderedMap[[]*TreeNode]()}
}

// Add generates transfer patterns from a kConnections index and merges them.
func (g *GraphResults) Add(kConnections *ConnectionIndex) {
	for _, path := range getPaths(kConnections) {
		g.mergePath(path)
	}
}

// Finalize returns the graph.
func (g *GraphResults) Finalize() *OrderedMap[[]*TreeNode] {
	return g.results
}

func getPaths(kConnections *ConnectionIndex) []Path {
	results := []Path{}
	for _, destination := range kConnections.Stops() {
		round := kConnections.Round(destination)
		for _, k := range round.rounds {
			results = append(results, getGraphPath(kConnections, k, destination))
		}
	}
	return results
}

func getGraphPath(kConnections *ConnectionIndex, k int, finalDestination StopID) Path {
	path := Path{finalDestination}
	destination := finalDestination
	for i := k; i > 0; i-- {
		connection := kConnections.Round(destination).get(i)
		var origin StopID
		if connection.IsTransfer() {
			origin = connection.Transfer.Origin
		} else {
			origin = connection.Trip.StopTimes[connection.StartIndex].Stop
		}
		path = append(path, origin)
		destination = origin
	}
	return path
}

func (g *GraphResults) mergePath(path Path) *TreeNode {
	head := path[0]
	tail := path[1:]

	nodes, _ := g.results.Get(head)

	var node *TreeNode
	for _, n := range nodes {
		if isSame(tail, n.Parent) {
			node = n
			break
		}
	}

	if node == nil {
		var parent *TreeNode
		if len(tail) > 0 {
			parent = g.mergePath(tail)
		}
		node = &TreeNode{Label: head, Parent: parent}
		g.results.Set(head, append(nodes, node))
	}

	return node
}

func isSame(path Path, node *TreeNode) bool {
	i := 0
	for node != nil {
		if i >= len(path) || node.Label != path[i] {
			return false
		}
		i++
		node = node.Parent
	}
	return true
}
