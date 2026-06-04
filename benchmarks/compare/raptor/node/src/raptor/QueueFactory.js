/**
 * Create a queue for the Raptor algorithm to use on each iteration of the algorithm.
 */
export class QueueFactory {
  constructor(routesAtStop, routeStopIndex) {
    this.routesAtStop = routesAtStop;
    this.routeStopIndex = routeStopIndex;
  }

  /**
   * Take the marked stops and return an index of any routes that pass through those stops.
   */
  getQueue(markedStops) {
    const queue = Object.create(null);

    for (const stop of markedStops) {
      for (const routeId of this.routesAtStop[stop] || []) {
        queue[routeId] = (queue[routeId] && this.isStopBefore(routeId, queue[routeId], stop)) ? queue[routeId] : stop;
      }
    }

    return queue;
  }

  isStopBefore(routeId, stopA, stopB) {
    return this.routeStopIndex[routeId][stopA] < this.routeStopIndex[routeId][stopB];
  }
}
