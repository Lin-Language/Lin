package raptor

// RoutesIndexedByStop maps a stop to the routes that pass through it (in the
// order they were registered).
type RoutesIndexedByStop = map[StopID][]RouteID

// RouteStopIndex maps a route to the index of each stop within its path.
type RouteStopIndex = map[RouteID]map[StopID]int

// QueueFactory builds the per-round route queue from a set of marked stops.
type QueueFactory struct {
	routesAtStop   RoutesIndexedByStop
	routeStopIndex RouteStopIndex
}

// NewQueueFactory constructs a QueueFactory.
func NewQueueFactory(routesAtStop RoutesIndexedByStop, routeStopIndex RouteStopIndex) *QueueFactory {
	return &QueueFactory{routesAtStop: routesAtStop, routeStopIndex: routeStopIndex}
}

// GetQueue returns an insertion-ordered map of routeId -> earliest marked stop.
// The reference builds a JS object and later iterates it with Object.entries,
// so insertion order is load-bearing.
func (f *QueueFactory) GetQueue(markedStops []StopID) *OrderedMap[StopID] {
	queue := NewOrderedMap[StopID]()

	for _, stop := range markedStops {
		for _, routeId := range f.routesAtStop[stop] {
			if existing, ok := queue.Get(routeId); ok && f.isStopBefore(routeId, existing, stop) {
				queue.Set(routeId, existing)
			} else {
				queue.Set(routeId, stop)
			}
		}
	}

	return queue
}

func (f *QueueFactory) isStopBefore(routeId RouteID, stopA, stopB StopID) bool {
	return f.routeStopIndex[routeId][stopA] < f.routeStopIndex[routeId][stopB]
}
