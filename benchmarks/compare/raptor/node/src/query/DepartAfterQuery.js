import { GroupStationDepartAfterQuery } from "./GroupStationDepartAfterQuery.js";

/**
 * Implementation of Raptor that searches for journeys departing after a specific time.
 *
 * Only returns results from a single pass of the Raptor algorithm. No filters are applied.
 */
export class DepartAfterQuery {
  constructor(raptor, resultsFactory, maxSearchDays = 3) {
    this.raptor = raptor;
    this.resultsFactory = resultsFactory;
    this.maxSearchDays = maxSearchDays;
    this.groupQuery = new GroupStationDepartAfterQuery(raptor, resultsFactory, maxSearchDays);
  }

  /**
   * Plan a journey between the origin and destination on the given date and time.
   */
  plan(origin, destination, date, time) {
    return this.groupQuery.plan([origin], [destination], date, time);
  }
}
