package raptor

import "testing"

func TestService(t *testing.T) {
	t.Run("checks the start date", func(t *testing.T) {
		s := NewService(20181001, 20181015, allDays, map[DateNumber]bool{})
		if s.RunsOn(20180930, 1) != false {
			t.Fatal("expected false")
		}
	})

	t.Run("checks the end date", func(t *testing.T) {
		s := NewService(20181001, 20181015, allDays, map[DateNumber]bool{})
		if s.RunsOn(20181016, 1) != false {
			t.Fatal("expected false")
		}
	})

	t.Run("checks dates within range", func(t *testing.T) {
		s := NewService(20181001, 20181015, allDays, map[DateNumber]bool{})
		if s.RunsOn(20181010, 1) != true {
			t.Fatal("expected true")
		}
	})

	t.Run("checks the day of the week", func(t *testing.T) {
		days := map[DayOfWeek]bool{0: true, 1: false, 2: true, 3: true, 4: true, 5: true, 6: true}
		s := NewService(20181001, 20991231, days, map[DateNumber]bool{})
		if s.RunsOn(20181016, 1) != false {
			t.Fatal("expected false")
		}
	})

	t.Run("checks include days", func(t *testing.T) {
		s := NewService(20991231, 20991231, allDays, map[DateNumber]bool{20181022: true})
		if s.RunsOn(20181022, 1) != true {
			t.Fatal("expected true")
		}
	})

	t.Run("checks exclude days", func(t *testing.T) {
		s := NewService(20181001, 20991231, allDays, map[DateNumber]bool{20181022: false})
		if s.RunsOn(20181022, 1) != false {
			t.Fatal("expected false")
		}
	})
}
