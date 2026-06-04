import { GroupStationDepartAfterQuery } from "./GroupStationDepartAfterQuery.js";

const ONE_DAY = 24 * 60 * 60;

/**
 * Use the Raptor algorithm to generate a full day of results.
 */
export class RangeQuery {
  constructor(raptor, resultsFactory, maxSearchDays = 3, filters = []) {
    this.raptor = raptor;
    this.resultsFactory = resultsFactory;
    this.maxSearchDays = maxSearchDays;
    this.filters = filters;
    this.groupQuery = new GroupStationDepartAfterQuery(raptor, resultsFactory, maxSearchDays);
  }

  /**
   * Perform a query at midnight, and then continue to search one minute after the earliest departure of each set of
   * results.
   */
  plan(origin, destination, date, time = 1, endTime = ONE_DAY) {
    const results = [];

    while (time < endTime) {
      const newResults = this.groupQuery.plan([origin], [destination], date, time);

      results.push(...newResults);

      if (newResults.length === 0) {
        break;
      }

      time = Math.min(...newResults.map(j => j.departureTime)) + 1;
    }

    return this.filters.reduce((rs, filter) => filter.apply(rs), results);
  }
}
