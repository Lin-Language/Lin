package raptor

import "testing"

func TestTimeParser(t *testing.T) {
	t.Run("turns a time string into seconds from midnight", func(t *testing.T) {
		parser := NewTimeParser()
		cases := []struct {
			in   string
			want Time
		}{
			{"00:00:00", 0},
			{"00:00:10", 10},
			{"00:02:10", 130},
			{"03:02:10", 10930},
		}
		for _, c := range cases {
			if got := parser.GetTime(c.in); got != c.want {
				t.Errorf("GetTime(%q) = %d, want %d", c.in, got, c.want)
			}
		}
	})
}
