package raptor

// Service models a GTFS calendar with include/exclude date overrides.
//
// The dates index distinguishes "key present" from "truthy" exactly like the
// reference: a present key with value true includes that date (and
// short-circuits), a present key with value false excludes it, an absent key
// falls through to the start/end/day-of-week check.
type Service struct {
	StartDate DateNumber
	EndDate   DateNumber
	Days      map[DayOfWeek]bool
	Dates     map[DateNumber]bool
}

// NewService builds a Service.
func NewService(startDate, endDate DateNumber, days map[DayOfWeek]bool, dates map[DateNumber]bool) *Service {
	if days == nil {
		days = map[DayOfWeek]bool{}
	}
	if dates == nil {
		dates = map[DateNumber]bool{}
	}
	return &Service{StartDate: startDate, EndDate: endDate, Days: days, Dates: dates}
}

// RunsOn replicates:
//
//	dates[date] === true OR
//	  (!hasOwn(dates, date) && startDate <= date <= endDate && days[dow])
func (s *Service) RunsOn(date DateNumber, dow DayOfWeek) bool {
	if v, ok := s.Dates[date]; ok {
		// Key present: its truthiness is the value. The JS `||` short-circuits
		// here regardless of whether it is true or false (a false value makes
		// the !hasOwn clause false, so the whole expression is false).
		return v
	}

	return s.StartDate <= date && s.EndDate >= date && s.Days[dow]
}
