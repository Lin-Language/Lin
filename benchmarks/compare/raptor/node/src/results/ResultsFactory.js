/**
 * Type check for a kConnection connection. A Transfer has an `origin` field;
 * a timetable connection is a [trip, start, end] tuple.
 */
export function isTransfer(connection) {
  return connection.origin !== undefined;
}
