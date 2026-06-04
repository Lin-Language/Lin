package raptor

import "testing"

func TestMultipleCriteriaFilter(t *testing.T) {
	filter := NewMultipleCriteriaFilter()

	t.Run("removes slower journeys", func(t *testing.T) {
		journeys := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(900)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1100)), st("B", p(1130), p(1135)), st("C", p(1200), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("C", p(1300), nil)}),
		}
		expected := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1100)), st("B", p(1130), p(1135)), st("C", p(1200), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("C", p(1300), nil)}),
		}
		assertJourneys(t, filter.Apply(journeys), expected)
	})

	t.Run("keeps slower journeys if they have fewer changes", func(t *testing.T) {
		journeys := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035))}, []StopTime{st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(900)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1100)), st("B", p(1130), p(1135)), st("C", p(1200), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("C", p(1300), nil)}),
		}
		expected := []Journey{
			j([]StopTime{st("A", nil, p(900)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035))}, []StopTime{st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1100)), st("B", p(1130), p(1135)), st("C", p(1200), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("C", p(1300), nil)}),
		}
		assertJourneys(t, filter.Apply(journeys), expected)
	})

	t.Run("sorts journeys before filtering them", func(t *testing.T) {
		journeys := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(900)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1100)), st("B", p(1130), p(1135)), st("C", p(1200), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("C", p(1300), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("C", p(1330), nil)}),
		}
		expected := []Journey{
			j([]StopTime{st("A", nil, p(1000)), st("B", p(1030), p(1035)), st("C", p(1100), nil)}),
			j([]StopTime{st("A", nil, p(1100)), st("B", p(1130), p(1135)), st("C", p(1200), nil)}),
			j([]StopTime{st("A", nil, p(1200)), st("B", p(1230), p(1235)), st("C", p(1300), nil)}),
		}
		assertJourneys(t, filter.Apply(journeys), expected)
	})
}
