/**
 * Convert a Date object into a numeric representation e.g. 20190417 (in UTC).
 */
export function getDateNumber(date) {
  const str = date.toISOString();

  return parseInt(str.slice(0, 4) + str.slice(5, 7) + str.slice(8, 10), 10);
}
