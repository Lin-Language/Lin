/**
 * Returns true if b arrives before or at the same time as a
 */
export const earliestArrival = (a, b) => b.arrivalTime <= a.arrivalTime;

/**
 * Returns true if b has the same or fewer changes than a
 */
export const leastChanges = (a, b) => b.legs.length <= a.legs.length;

/**
 * Filters journeys based on a number of configurable criteria
 */
export class MultipleCriteriaFilter {
  constructor(criteria = [earliestArrival, leastChanges]) {
    this.criteria = criteria;
  }

  /**
   * Sort the journeys and then apply the criteria
   */
  apply(journeys) {
    journeys.sort(this.sort);

    return journeys.filter((a, i, js) => this.compare(a, i, js));
  }

  /**
   * Sort by departure time ascending and arrival time descending as a tie breaker
   */
  sort(a, b) {
    return a.departureTime !== b.departureTime ? a.departureTime - b.departureTime : b.arrivalTime - a.arrivalTime;
  }

  /**
   * Keeps the journey as long as there is no subsequent journey that is better in every regard.
   */
  compare(journeyA, index, journeys) {
    for (let j = index + 1; j < journeys.length; j++) {
      const journeyB = journeys[j];

      if (this.criteria.every(criteria => criteria(journeyA, journeyB))) {
        return false;
      }
    }

    return true;
  }
}
