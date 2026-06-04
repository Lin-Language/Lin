import { describe, it } from "node:test";
import { expect } from "./expect.js";
import { j, setDefaultTrip, st, t } from "./util.js";
import { JourneyFactory } from "../src/results/JourneyFactory.js";
import { RaptorAlgorithmFactory } from "../src/raptor/RaptorAlgorithmFactory.js";
import { RangeQuery } from "../src/query/RangeQuery.js";

describe("RangeQuery", () => {
  const journeyFactory = new JourneyFactory();

  it("performs profile queries", () => {
    const trips = [
      t(st("A", null, 1000), st("B", 1030, 1035), st("C", 1100, null)),
      t(st("A", null, 1100), st("B", 1130, 1135), st("C", 1200, null)),
      t(st("A", null, 1200), st("B", 1230, 1235), st("C", 1300, null))
    ];

    const raptor = RaptorAlgorithmFactory.create(trips, {}, {});
    const query = new RangeQuery(raptor, journeyFactory);
    const result = query.plan("A", "C", new Date("2018-10-16"));

    setDefaultTrip(result);

    expect(result).toEqual([
      j([st("A", null, 1000), st("B", 1030, 1035), st("C", 1100, null)]),
      j([st("A", null, 1100), st("B", 1130, 1135), st("C", 1200, null)]),
      j([st("A", null, 1200), st("B", 1230, 1235), st("C", 1300, null)])
    ]);
  });

  it("does not share bestArrivals or routeScanner", () => {
    const trips = [
      t(st("A", null, 1359), st("C", 1501, null)),
      t(st("A", null, 1400), st("B", 1430, null)),
      t(st("B", null, 1430), st("C", 1500, null))
    ];

    const raptor = RaptorAlgorithmFactory.create(trips, {}, {});
    const query = new RangeQuery(raptor, journeyFactory);
    const result = query.plan("A", "C", new Date("2018-10-16"));

    setDefaultTrip(result);

    expect(result).toEqual([
      j([st("A", null, 1359), st("C", 1501, null)]),
      j([st("A", null, 1400), st("B", 1430, null)], [st("B", null, 1430), st("C", 1500, null)]),
      j([st("A", null, 1400), st("B", 1430, null)], [st("B", null, 1430), st("C", 1500, null)]),
    ]);
  });
});
