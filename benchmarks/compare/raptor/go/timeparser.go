package raptor

import (
	"strconv"
	"strings"
)

// TimeParser parses "HH:MM:SS" strings into seconds from midnight, caching
// results.
type TimeParser struct {
	cache map[string]Time
}

// NewTimeParser constructs a TimeParser with an empty cache.
func NewTimeParser() *TimeParser {
	return &TimeParser{cache: map[string]Time{}}
}

// GetTime converts a time string to seconds from midnight.
func (p *TimeParser) GetTime(time string) Time {
	if v, ok := p.cache[time]; ok {
		return v
	}

	parts := strings.Split(time, ":")
	hh, _ := strconv.ParseInt(parts[0], 10, 64)
	mm, _ := strconv.ParseInt(parts[1], 10, 64)
	ss, _ := strconv.ParseInt(parts[2], 10, 64)

	v := hh*60*60 + mm*60 + ss
	p.cache[time] = v
	return v
}
