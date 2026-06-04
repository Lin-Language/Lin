package raptor

import "testing"

func TestRangeQuery(t *testing.T) {
	journeyFactory := NewJourneyFactory()

	t.Run("performs profile queries", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)),
			tripOf(st("A", nil, p(1100)), st("B", p(1130), p(1135)), st("C", p(1200), nil)),
			tripOf(st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("C", p(1300), nil)),
		}
		raptor := CreateRaptorAlgorithm(trips, nil, nil, nil)
		q := NewRangeQuery(raptor, journeyFactory, 0, nil)
		result := q.Plan("A", "C", NewSearchDate("2018-10-16"), 0, 0)
		setDefaultTrip(result)

		expected := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1100)), st("B", p(1130), p(1135)), st("C", p(1200), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("C", p(1300), nil)}),
		}
		assertJourneys(t, result, expected)
	})

	t.Run("does not share bestArrivals or routeScanner", func(t *testing.T) {
		trips := []*Trip{
			tripOf(st("A", nil, p(1359)), st("C", p(1501), nil)),
			tripOf(st("A", nil, p(1400)), st("B", p(1430), nil)),
			tripOf(st("B", nil, p(1430)), st("C", p(1500), nil)),
		}
		raptor := CreateRaptorAlgorithm(trips, nil, nil, nil)
		q := NewRangeQuery(raptor, journeyFactory, 0, nil)
		result := q.Plan("A", "C", NewSearchDate("2018-10-16"), 0, 0)
		setDefaultTrip(result)

		ac := func() Journey {
			return j([]StopTime{st("A", nil, p(1400)), st("B", p(1430), nil)},
				[]StopTime{st("B", nil, p(1430)), st("C", p(1500), nil)})
		}
		expected := []Journey{
			j([]StopTime{st("A", nil, p(1359)), st("C", p(1501), nil)}),
			ac(),
			ac(),
		}
		assertJourneys(t, result, expected)
	})
}
