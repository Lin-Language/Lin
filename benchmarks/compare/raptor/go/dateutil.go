package raptor

import "time"

// SearchDate models the JS Date used by the queries: it is always interpreted
// in UTC, can be advanced by whole days (with month/year rollover), and yields
// both a YYYYMMDD DateNumber and a JS-style day-of-week.
type SearchDate struct {
	t time.Time
}

// NewSearchDate parses an ISO date string ("2018-10-16") as UTC midnight,
// mirroring `new Date("2018-10-16")` in JS.
func NewSearchDate(iso string) SearchDate {
	t, err := time.Parse("2006-01-02", iso)
	if err != nil {
		panic("invalid date: " + iso)
	}
	return SearchDate{t: t.UTC()}
}

// GetDateNumber returns the date as a YYYYMMDD integer in UTC, mirroring
// getDateNumber(date) which slices date.toISOString().
func (d SearchDate) GetDateNumber() DateNumber {
	y, m, day := d.t.Date()
	return y*10000 + int(m)*100 + day
}

// DayOfWeek returns Sunday=0..Saturday=6 (JS Date.getDay) computed from UTC.
func (d SearchDate) DayOfWeek() DayOfWeek {
	return int(d.t.Weekday())
}

// AddDay returns a new SearchDate advanced by one calendar day, handling
// month / year rollover.
func (d SearchDate) AddDay() SearchDate {
	return SearchDate{t: d.t.AddDate(0, 0, 1)}
}
