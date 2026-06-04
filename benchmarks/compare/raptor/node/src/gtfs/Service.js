/**
 * Service calendar logic.
 *
 * dates is the include/exclude index: a key present with value true = include,
 * present with false = exclude.
 */
export class Service {
  constructor(startDate, endDate, days, dates) {
    this.startDate = startDate;
    this.endDate = endDate;
    this.days = days;
    this.dates = dates;
  }

  runsOn(date, dow) {
    return this.dates[date] || (
      !Object.hasOwn(this.dates, date) &&
      this.startDate <= date &&
      this.endDate >= date &&
      this.days[dow]
    );
  }
}
