package raptor

import (
	"bufio"
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
)

// Interchange and TransfersByOrigin are defined alongside the algorithm types.

// csvRow yields the header column-name -> index lookup for a file.
func indexOfColumns(header []string) map[string]int {
	idx := make(map[string]int, len(header))
	for i, name := range header {
		idx[name] = i
	}
	return idx
}

// scanCsv opens path and calls fn for each data row (header excluded). Lines are
// split on comma (no quoted fields in this feed). Empty lines are skipped.
// The header column-name -> index map is passed to fn so columns are read by
// name, independent of incidental column order (mirroring the reference).
func scanCsv(path string, fn func(c map[string]int, fields []string)) error {
	f, err := os.Open(path)
	if err != nil {
		return err
	}
	defer f.Close()

	sc := bufio.NewScanner(f)
	// stop_times.txt is ~93MB / 2.37M rows; lines are short but give the scanner
	// a generous buffer to be safe.
	sc.Buffer(make([]byte, 0, 1024*1024), 16*1024*1024)

	if !sc.Scan() {
		return sc.Err()
	}
	c := indexOfColumns(strings.Split(sc.Text(), ","))

	for sc.Scan() {
		line := sc.Text()
		if len(line) == 0 {
			continue
		}
		fn(c, strings.Split(line, ","))
	}
	return sc.Err()
}

// atoiInt parses an integer field (e.g. a YYYYMMDD date) as int.
func atoiInt(s string) int {
	v, _ := strconv.Atoi(s)
	return v
}

// LoadGTFS reads the GTFS CSV files in dataDir and returns trips, transfers and
// interchange, mirroring GTFSLoader.ts: trips carry stopTimes in file order;
// Service is built from calendar.txt + calendar_dates.txt; transfers come from
// transfers.txt (same-stop -> interchange, else a Transfer) plus links.txt
// footpaths (date/day columns ignored). Trips whose serviceId has no calendar
// row are dropped and the count reported to stderr.
func LoadGTFS(dataDir string) ([]*Trip, TransfersByOrigin, Interchange, error) {
	tp := NewTimeParser()

	type calRow struct {
		serviceID ServiceID
		startDate DateNumber
		endDate   DateNumber
		days      map[DayOfWeek]bool
	}

	calendars := map[ServiceID]*calRow{}
	dates := map[ServiceID]map[DateNumber]bool{}
	stopTimes := map[TripID][]StopTime{}
	transfers := TransfersByOrigin{}
	interchange := Interchange{}
	var trips []*Trip

	// --- calendar.txt ---
	if err := scanCsv(filepath.Join(dataDir, "calendar.txt"), func(c map[string]int, r []string) {
		serviceID := r[c["service_id"]]
		calendars[serviceID] = &calRow{
			serviceID: serviceID,
			startDate: atoiInt(r[c["start_date"]]),
			endDate:   atoiInt(r[c["end_date"]]),
			days: map[DayOfWeek]bool{
				0: r[c["sunday"]] == "1",
				1: r[c["monday"]] == "1",
				2: r[c["tuesday"]] == "1",
				3: r[c["wednesday"]] == "1",
				4: r[c["thursday"]] == "1",
				5: r[c["friday"]] == "1",
				6: r[c["saturday"]] == "1",
			},
		}
	}); err != nil {
		return nil, nil, nil, fmt.Errorf("calendar.txt: %w", err)
	}

	// --- calendar_dates.txt: dates[service_id][+date] = (exception_type === "1") ---
	if err := scanCsv(filepath.Join(dataDir, "calendar_dates.txt"), func(c map[string]int, r []string) {
		serviceID := r[c["service_id"]]
		date := atoiInt(r[c["date"]])
		include := r[c["exception_type"]] == "1"
		m := dates[serviceID]
		if m == nil {
			m = map[DateNumber]bool{}
			dates[serviceID] = m
		}
		m[date] = include
	}); err != nil {
		return nil, nil, nil, fmt.Errorf("calendar_dates.txt: %w", err)
	}

	// --- stop_times.txt: grouped by trip_id, in file order ---
	if err := scanCsv(filepath.Join(dataDir, "stop_times.txt"), func(c map[string]int, r []string) {
		tripID := r[c["trip_id"]]
		pickupType := r[c["pickup_type"]]
		dropOffType := r[c["drop_off_type"]]
		st := StopTime{
			Stop:          r[c["stop_id"]],
			DepartureTime: tp.GetTime(r[c["departure_time"]]),
			ArrivalTime:   tp.GetTime(r[c["arrival_time"]]),
			// "0" or empty => true; "1"/"3" => false (matches reference).
			PickUp:  pickupType == "0" || pickupType == "",
			DropOff: dropOffType == "0" || dropOffType == "",
		}
		stopTimes[tripID] = append(stopTimes[tripID], st)
	}); err != nil {
		return nil, nil, nil, fmt.Errorf("stop_times.txt: %w", err)
	}

	// --- trips.txt (stopTimes/service resolved after) ---
	if err := scanCsv(filepath.Join(dataDir, "trips.txt"), func(c map[string]int, r []string) {
		trips = append(trips, &Trip{
			ServiceID: r[c["service_id"]],
			TripID:    r[c["trip_id"]],
		})
	}); err != nil {
		return nil, nil, nil, fmt.Errorf("trips.txt: %w", err)
	}

	// --- transfers.txt: interchange + transfers ---
	if err := scanCsv(filepath.Join(dataDir, "transfers.txt"), func(c map[string]int, r []string) {
		from := r[c["from_stop_id"]]
		to := r[c["to_stop_id"]]
		if from == to {
			interchange[from] = Time(atoiInt(r[c["min_transfer_time"]]))
		} else {
			transfers[from] = append(transfers[from], &Transfer{
				Origin:      from,
				Destination: to,
				Duration:    Duration(atoiInt(r[c["min_transfer_time"]])),
				StartTime:   0,
				EndTime:     MaxSafeInteger,
			})
		}
	}); err != nil {
		return nil, nil, nil, fmt.Errorf("transfers.txt: %w", err)
	}

	// --- links.txt: footpaths (date/day columns ignored, matching reference) ---
	if err := scanCsv(filepath.Join(dataDir, "links.txt"), func(c map[string]int, r []string) {
		from := r[c["from_stop_id"]]
		transfers[from] = append(transfers[from], &Transfer{
			Origin:      from,
			Destination: r[c["to_stop_id"]],
			Duration:    Duration(atoiInt(r[c["duration"]])),
			StartTime:   tp.GetTime(r[c["start_time"]]),
			EndTime:     tp.GetTime(r[c["end_time"]]),
		})
	}); err != nil {
		return nil, nil, nil, fmt.Errorf("links.txt: %w", err)
	}

	// --- Service resolution ---
	services := map[ServiceID]*Service{}
	for serviceID, cal := range calendars {
		services[serviceID] = NewService(cal.startDate, cal.endDate, cal.days, dates[serviceID])
	}

	// Resolve stopTimes + service per trip; drop trips whose serviceId has no
	// calendar row.
	resolved := trips[:0]
	dropped := 0
	for _, t := range trips {
		service := services[t.ServiceID]
		if service == nil {
			dropped++
			continue
		}
		t.StopTimes = stopTimes[t.TripID]
		t.Service = service
		resolved = append(resolved, t)
	}

	if dropped > 0 {
		fmt.Fprintf(os.Stderr, "dropped %d trip(s) with no calendar row\n", dropped)
	}

	return resolved, transfers, interchange, nil
}
