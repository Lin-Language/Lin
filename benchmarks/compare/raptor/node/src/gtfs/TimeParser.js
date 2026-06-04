/**
 * Parses time strings and returns them as seconds from midnight. Caches results.
 */
export class TimeParser {
  constructor() {
    this.timeCache = {};
  }

  /**
   * Convert a time string to seconds from midnight
   */
  getTime(time) {
    if (!Object.hasOwn(this.timeCache, time)) {
      const [hh, mm, ss] = time.split(":");

      this.timeCache[time] = (+hh) * 60 * 60 + (+mm) * 60 + (+ss);
    }

    return this.timeCache[time];
  }
}
