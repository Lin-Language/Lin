package raptor

import "testing"

func TestGroupStationDepartAfterQuery(t *testing.T) {
	journeyFactory := NewJourneyFactory()
	makeFilters := func() []JourneyFilter { return []JourneyFilter{NewMultipleCriteriaFilter()} }

	t.Run("plans to multiple destinations", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)),
			tripOf(st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("D", p(1300), nil)),
		}
		raptor := CreateRaptorAlgorithm(trips, nil, nil, nil)
		q := NewGroupStationDepartAfterQuery(raptor, journeyFactory, 1, makeFilters())
		result := q.Plan([]StopID{"A"}, []StopID{"C", "D"}, NewSearchDate("2019-04-18"), 900)
		setDefaultTrip(result)

		expected := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("D", p(1300), nil)}),
		}
		assertJourneys(t, result, expected)
	})

	t.Run("plans from multiple origins", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)),
			tripOf(st("A", nil, p(1200)), st("_", p(1230), p(1235)), st("D", p(1300), nil)),
		}
		raptor := CreateRaptorAlgorithm(trips, nil, nil, nil)
		q := NewGroupStationDepartAfterQuery(raptor, journeyFactory, 1, makeFilters())
		result := q.Plan([]StopID{"A", "B"}, []StopID{"C", "D"}, NewSearchDate("2019-04-18"), 900)
		setDefaultTrip(result)

		expected := []Journey{
			j([]StopTime{st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("_", p(1230), p(1235)), st("D", p(1300), nil)}),
		}
		assertJourneys(t, result, expected)
	})
}
