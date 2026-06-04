import { isTransfer } from "./ResultsFactory.js";

/**
 * Extracts journeys from the kConnections index.
 */
export class JourneyFactory {
  /**
   * Take the best result of each round for the given destination and turn it into a journey.
   */
  getResults(kConnections, destination) {
    const results = [];

    // Object.keys over an integer-keyed object yields keys in numeric-ascending order.
    for (const k of Object.keys(kConnections[destination] || {})) {
      const legs = this.getJourneyLegs(kConnections, k, destination);
      const departureTime = this.getDepartureTime(legs);
      const arrivalTime = this.getArrivalTime(legs);

      results.push({ legs, departureTime, arrivalTime });
    }

    return results;
  }

  /**
   * Iterate back through each connection and build up a series of legs to plan the journey
   */
  getJourneyLegs(kConnections, k, finalDestination) {
    const legs = [];

    for (let destination = finalDestination, i = parseInt(k, 10); i > 0; i--) {
      const connection = kConnections[destination][i];

      if (isTransfer(connection)) {
        legs.push(connection);

        destination = connection.origin;
      } else {
        const [trip, start, end] = connection;
        const stopTimes = trip.stopTimes.slice(start, end + 1);
        const origin = stopTimes[0].stop;

        legs.push({ stopTimes, origin, destination, trip });

        destination = origin;
      }
    }

    return legs.reverse();
  }

  getDepartureTime(legs) {
    let transferDuration = 0;

    for (const leg of legs) {
      if (!this.isTimetableLeg(leg)) {
        transferDuration += leg.duration;
      }
      else {
        return leg.stopTimes[0].departureTime - transferDuration;
      }
    }

    return 0;
  }

  getArrivalTime(legs) {
    let transferDuration = 0;

    for (let i = legs.length - 1; i >= 0; i--) {
      const leg = legs[i];

      if (!this.isTimetableLeg(leg)) {
        transferDuration += leg.duration;
      }
      else {
        return leg.stopTimes[leg.stopTimes.length - 1].arrivalTime + transferDuration;
      }
    }

    return 0;
  }

  isTimetableLeg(connection) {
    return connection.stopTimes !== undefined;
  }
}
