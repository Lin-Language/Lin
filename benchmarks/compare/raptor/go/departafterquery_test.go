package raptor

import "testing"

func TestDepartAfterQuery(t *testing.T) {
	journeyFactory := NewJourneyFactory()

	plan := func(trips []*Trip, transfers TransfersByOrigin, interchange Interchange, origin, dest, date string, time Time, maxDays int) []Journey {
		raptor := CreateRaptorAlgorithm(trips, transfers, interchange, nil)
		q := NewDepartAfterQuery(raptor, journeyFactory, maxDays)
		d := NewSearchDate(date)
		result := q.Plan(origin, dest, d, time)
		setDefaultTrip(result)
		return result
	}

	t.Run("finds journeys with direct connections", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)),
		}
		result := plan(trips, nil, nil, "A", "C", "2018-10-16", 900, 0)
		expected := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
		}
		assertJourneys(t, result, expected)
	})

	t.Run("finds the earliest calendars", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1400)), st("B", p(1430), p(1435)), st("C", p(1500), nil)),
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)),
		}
		result := plan(trips, nil, nil, "A", "C", "2018-10-16", 900, 0)
		expected := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
		}
		assertJourneys(t, result, expected)
	})

	t.Run("finds journeys with a single connection", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)),
			tripOf(st("D", nil, p(1000)), st("B", p(1030), p(1035)), st("E", p(1100), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2018-10-16", 900, 0)
		expected := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035))},
				[]StopTime{st("B", p(1030), p(1035)), st("E", p(1100), nil)}),
		}
		assertJourneys(t, result, expected)
	})

	t.Run("does not return journeys that cannot be made", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1035), p(1035)), st("C", p(1100), nil)),
			tripOf(st("D", nil, p(1000)), st("B", p(1030), p(1030)), st("E", p(1100), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2018-10-16", 900, 1)
		assertJourneys(t, result, []Journey{})
	})

	t.Run("returns the fastest and the least changes", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1200), nil)),
			tripOf(st("B", nil, p(1030)), st("C", p(1100), nil)),
		}
		result := plan(trips, nil, nil, "A", "C", "2018-10-16", 900, 0)
		direct := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1200), nil)})
		change := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1030))},
			[]StopTime{st("B", nil, p(1030)), st("C", p(1100), nil)})
		assertJourneys(t, result, []Journey{direct, change})
	})

	t.Run("chooses the fastest journey where the number of journeys is the same", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1100), nil)),
			tripOf(st("C", nil, p(1200)), st("D", p(1230), p(1230)), st("E", p(1300), nil)),
			tripOf(st("A", nil, p(1100)), st("F", p(1130), p(1130)), st("G", p(1200), nil)),
			tripOf(st("G", nil, p(1200)), st("H", p(1230), p(1230)), st("E", p(1255), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2018-10-16", 900, 0)
		fastest := j([]StopTime{st("A", nil, p(1100)), st("F", p(1130), p(1130)), st("G", p(1200), nil)},
			[]StopTime{st("G", nil, p(1200)), st("H", p(1230), p(1230)), st("E", p(1255), nil)})
		assertJourneys(t, result, []Journey{fastest})
	})

	t.Run("chooses an arbitrary journey when they are the same", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1100), nil)),
			tripOf(st("C", nil, p(1200)), st("D", p(1230), p(1230)), st("E", p(1300), nil)),
			tripOf(st("A", nil, p(1100)), st("F", p(1130), p(1130)), st("G", p(1200), nil)),
			tripOf(st("G", nil, p(1200)), st("H", p(1230), p(1230)), st("E", p(1300), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2018-10-16", 900, 0)
		journey1 := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1100), nil)},
			[]StopTime{st("C", nil, p(1200)), st("D", p(1230), p(1230)), st("E", p(1300), nil)})
		assertJourneys(t, result, []Journey{journey1})
	})

	t.Run("chooses the correct change point", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), nil)),
			tripOf(st("A", nil, p(1030)), st("C", p(1200), nil)),
			tripOf(st("C", nil, p(1000)), st("B", p(1030), p(1030)), st("E", p(1100), nil)),
			tripOf(st("C", nil, p(1200)), st("B", p(1230), p(1230)), st("E", p(1300), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2018-10-16", 900, 0)
		change := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), nil)},
			[]StopTime{st("B", p(1030), p(1030)), st("E", p(1100), nil)})
		assertJourneys(t, result, []Journey{change})
	})

	t.Run("finds journeys with a transfer", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)),
			tripOf(st("D", nil, p(1200)), st("E", p(1300), nil)),
		}
		transfers := TransfersByOrigin{"C": {tf("C", "D", 10)}}
		result := plan(trips, transfers, nil, "A", "E", "2018-10-16", 900, 0)
		expected := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)},
				tf("C", "D", 10),
				[]StopTime{st("D", nil, p(1200)), st("E", p(1300), nil)}),
		}
		assertJourneys(t, result, expected)
	})

	t.Run("uses a transfer if it is faster", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1100), nil)),
			tripOf(st("C", nil, p(1130)), st("D", p(1200), nil)),
		}
		transfers := TransfersByOrigin{"C": {tf("C", "D", 10)}}
		result := plan(trips, transfers, nil, "A", "D", "2018-10-16", 900, 0)
		transfer := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1100), nil)},
			tf("C", "D", 10))
		assertJourneys(t, result, []Journey{transfer})
	})

	t.Run("doesn't allow pick up from locations without pickup specified", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1200), nil)),
			tripOf(st("E", nil, p(1000)), st("B", p(1030), nil), st("C", p(1100), nil)),
		}
		result := plan(trips, nil, nil, "A", "C", "2018-10-16", 900, 0)
		direct := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1200), nil)})
		assertJourneys(t, result, []Journey{direct})
	})

	t.Run("doesn't allow drop off at non-drop off locations", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", nil, p(1030)), st("C", p(1200), nil)),
			tripOf(st("E", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1100), nil)),
		}
		result := plan(trips, nil, nil, "A", "C", "2018-10-16", 900, 0)
		direct := j([]StopTime{st("A", nil, p(1000)), st("B", nil, p(1030)), st("C", p(1200), nil)})
		assertJourneys(t, result, []Journey{direct})
	})

	t.Run("applies interchange times", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1200), nil)),
			tripOf(st("B", nil, p(1030)), st("C", p(1100), nil)),
			tripOf(st("B", nil, p(1040)), st("C", p(1110), nil)),
		}
		result := plan(trips, TransfersByOrigin{}, Interchange{"B": 10}, "A", "C", "2018-10-16", 900, 0)
		direct := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1200), nil)})
		change := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1030))},
			[]StopTime{st("B", nil, p(1040)), st("C", p(1110), nil)})
		assertJourneys(t, result, []Journey{direct, change})
	})

	t.Run("applies interchange times to transfers", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), nil)),
			tripOf(st("C", nil, p(1030)), st("D", p(1100), nil)),
			tripOf(st("C", nil, p(1050)), st("D", p(1110), nil)),
			tripOf(st("C", nil, p(1100)), st("D", p(1120), nil)),
		}
		transfers := TransfersByOrigin{"B": {tf("B", "C", 10)}}
		result := plan(trips, transfers, Interchange{"B": 10, "C": 10}, "A", "D", "2018-10-16", 900, 0)
		lastPossible := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), nil)},
			tf("B", "C", 10),
			[]StopTime{st("C", nil, p(1100)), st("D", p(1120), nil)})
		assertJourneys(t, result, []Journey{lastPossible})
	})

	t.Run("omits calendars not running that day", func(t *testing.T) {
		trip := tripOf(st("B", nil, p(1030)), st("C", p(1100), nil))
		trip.Service = NewService(20181001, 20181015, allDays, map[DateNumber]bool{})
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), nil)),
			trip,
			tripOf(st("B", nil, p(1040)), st("C", p(1110), nil)),
		}
		result := plan(trips, TransfersByOrigin{}, Interchange{}, "A", "C", "2018-10-16", 900, 0)
		change := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), nil)},
			[]StopTime{st("B", nil, p(1040)), st("C", p(1110), nil)})
		assertJourneys(t, result, []Journey{change})
	})

	t.Run("omits calendars not running that day of the week", func(t *testing.T) {
		trip := tripOf(st("B", nil, p(1030)), st("C", p(1100), nil))
		days := map[DayOfWeek]bool{0: true, 1: false, 2: true, 3: true, 4: true, 5: true, 6: true}
		trip.Service = NewService(20181001, 20991231, days, map[DateNumber]bool{})
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), nil)),
			trip,
			tripOf(st("B", nil, p(1040)), st("C", p(1110), nil)),
		}
		result := plan(trips, TransfersByOrigin{}, Interchange{}, "A", "C", "2018-10-22", 900, 0)
		change := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), nil)},
			[]StopTime{st("B", nil, p(1040)), st("C", p(1110), nil)})
		assertJourneys(t, result, []Journey{change})
	})

	t.Run("includes calendars with an include day", func(t *testing.T) {
		trip := tripOf(st("B", nil, p(1030)), st("C", p(1100), nil))
		trip.Service = NewService(20991231, 20991231, allDays, map[DateNumber]bool{20181022: true})
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), nil)),
			trip,
			tripOf(st("B", nil, p(1040)), st("C", p(1110), nil)),
		}
		result := plan(trips, TransfersByOrigin{}, Interchange{}, "A", "C", "2018-10-22", 900, 0)
		change := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), nil)},
			[]StopTime{st("B", nil, p(1030)), st("C", p(1100), nil)})
		assertJourneys(t, result, []Journey{change})
	})

	t.Run("omits calendars with an exclude day", func(t *testing.T) {
		trip := tripOf(st("B", nil, p(1030)), st("C", p(1100), nil))
		trip.Service = NewService(20181001, 20991231, allDays, map[DateNumber]bool{20181022: false})
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), nil)),
			trip,
			tripOf(st("B", nil, p(1040)), st("C", p(1110), nil)),
		}
		result := plan(trips, TransfersByOrigin{}, Interchange{}, "A", "C", "2018-10-22", 900, 0)
		change := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), nil)},
			[]StopTime{st("B", nil, p(1040)), st("C", p(1110), nil)})
		assertJourneys(t, result, []Journey{change})
	})

	t.Run("finds journeys after gaps in rounds", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1400), nil)),
			tripOf(st("B", nil, p(1035)), st("D", p(1100), nil)),
			tripOf(st("D", nil, p(1100)), st("E", p(1130), nil)),
			tripOf(st("E", nil, p(1130)), st("C", p(1200), nil)),
			tripOf(st("A", nil, p(1000)), st("E", p(1135), nil)),
			tripOf(st("E", nil, p(1135)), st("C", p(1330), nil)),
		}
		result := plan(trips, nil, nil, "A", "C", "2018-10-16", 900, 0)
		direct := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1400), nil)})
		slowChange := j([]StopTime{st("A", nil, p(1000)), st("E", p(1135), nil)},
			[]StopTime{st("E", nil, p(1135)), st("C", p(1330), nil)})
		change := j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035))},
			[]StopTime{st("B", nil, p(1035)), st("D", p(1100), nil)},
			[]StopTime{st("D", nil, p(1100)), st("E", p(1130), nil)},
			[]StopTime{st("E", nil, p(1130)), st("C", p(1200), nil)})
		assertJourneys(t, result, []Journey{direct, slowChange, change})
	})

	t.Run("puts overtaken trains in different routes", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1100), p(1110)), st("D", p(1130), p(1130)), st("E", p(1200), nil)),
			tripOf(st("A", nil, p(1010)), st("B", p(1040), p(1040)), st("C", p(1050), p(1100)), st("D", p(1120), p(1120)), st("E", p(1150), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2018-10-16", 900, 0)
		faster := j([]StopTime{st("A", nil, p(1010)), st("B", p(1040), p(1040)), st("C", p(1050), p(1100)), st("D", p(1120), p(1120)), st("E", p(1150), nil)})
		assertJourneys(t, result, []Journey{faster})
	})

	t.Run("finds journeys that can only be made by waiting for the next day", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1035), p(1035)), st("C", p(1100), nil)),
			tripOf(st("D", nil, p(1000)), st("B", p(1030), p(1030)), st("E", p(1100), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2018-10-16", 900, 2)
		expected := j([]StopTime{st("A", nil, p(1000)), st("B", p(1035), p(1035))},
			[]StopTime{st("B", p(1030), p(1030)), st("E", p(1100), nil)})
		if len(result) == 0 {
			t.Fatal("expected at least one journey")
		}
		if !legsEqual(result[0].Legs, expected.Legs) {
			t.Fatalf("legs mismatch:\n got %s\nwant %s", journeyString(result[0]), journeyString(expected))
		}
	})

	t.Run("adds a day to the arrival time of journeys that are made overnight", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1035), p(1035)), st("C", p(1100), nil)),
			tripOf(st("D", nil, p(1000)), st("B", p(1030), p(1030)), st("E", p(1100), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2018-10-16", 900, 2)
		if result[0].ArrivalTime != 1100+86400 {
			t.Fatalf("arrival = %d, want %d", result[0].ArrivalTime, 1100+86400)
		}
	})

	t.Run("increments the day when searching subsequent days", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1030)), st("C", p(1100), nil)),
			tripOf(st("D", nil, p(1000)), st("B", p(1035), p(1035)), st("E", p(1100), nil)),
		}
		trips[1].Service = services["2"]
		result := plan(trips, nil, nil, "A", "E", "2018-12-31", 900, 2)
		if result[0].ArrivalTime != 1100+86400 {
			t.Fatalf("arrival = %d, want %d", result[0].ArrivalTime, 1100+86400)
		}
	})

	t.Run("uses all results from every day", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1900)), st("B", p(1930), p(1935)), st("C", p(2000), nil)),
			tripOf(st("B", nil, p(1035)), st("D", p(1100), nil)),
			tripOf(st("D", nil, p(1100)), st("E", p(1130), nil)),
			tripOf(st("C", nil, p(1130)), st("E", p(1200), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2019-04-23", 900, 2)
		change := j([]StopTime{st("A", nil, p(1900)), st("B", p(1930), p(1935))},
			[]StopTime{st("B", nil, p(1035)), st("D", p(1100), nil)},
			[]StopTime{st("D", nil, p(1100)), st("E", p(1130), nil)})
		noChange := j([]StopTime{st("A", nil, p(1900)), st("B", p(1930), p(1935)), st("C", p(2000), nil)},
			[]StopTime{st("C", nil, p(1130)), st("E", p(1200), nil)})
		expected := []Journey{noChange, change}
		for i := range expected {
			expected[i].ArrivalTime += 86400
		}
		assertJourneys(t, result, expected)
	})

	t.Run("does not return overnight journeys that cannot be made", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(86000)), st("B", p(86400), p(86400)), st("C", p(86400+3600+3600), nil)),
			tripOf(st("C", nil, p(3600)), st("D", p(3635), p(3635)), st("E", p(3700), nil)),
			tripOf(st("C", nil, p(3600+3600)), st("D", p(3635+3600), p(3635+3600)), st("E", p(3700+3600), nil)),
		}
		result := plan(trips, nil, nil, "A", "E", "2018-12-31", 50000, 2)
		if result[0].ArrivalTime != 86400+3700+3600 {
			t.Fatalf("arrival = %d, want %d", result[0].ArrivalTime, 86400+3700+3600)
		}
	})
}
