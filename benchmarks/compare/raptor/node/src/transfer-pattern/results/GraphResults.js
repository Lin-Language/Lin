import { isTransfer } from "../../results/ResultsFactory.js";

/**
 * Uses the Raptor algorithm to perform full day range queries and stores the result as a DAG.
 */
export class GraphResults {
  constructor() {
    this.results = {};
  }

  /**
   * Generate transfer patterns and store them in a DAG
   */
  add(kConnections) {
    for (const path of this.getPaths(kConnections)) {
      this.mergePath(path);
    }
  }

  /**
   * Return the results
   */
  finalize() {
    return this.results;
  }

  getPaths(kConnections) {
    const results = [];

    for (const destination in kConnections) {
      for (const k in kConnections[destination]) {
        results.push(this.getPath(kConnections, k, destination));
      }
    }

    return results;
  }

  getPath(kConnections, k, finalDestination) {
    const path = [finalDestination];

    for (let destination = finalDestination, i = parseInt(k, 10); i > 0; i--) {
      const connection = kConnections[destination][i];
      const origin = isTransfer(connection) ? connection.origin : connection[0].stopTimes[connection[1]].stop;

      path.push(origin);

      destination = origin;
    }

    return path;
  }

  /**
   * Merge the given path into the transfer pattern graph.
   */
  mergePath([head, ...tail]) {
    this.results[head] = this.results[head] || [];

    let node = this.results[head].find(n => this.isSame(tail, n.parent));

    if (!node) {
      const parent = tail.length > 0 ? this.mergePath(tail) : null;

      node = { label: head, parent: parent };

      this.results[head].push(node);
    }

    return node;
  }

  /**
   * Check whether the given path is the same as the path between the given node and the root node
   */
  isSame(path, node) {
    for (let i = 0; node; i++, node = node.parent) {
      if (node.label !== path[i]) {
        return false;
      }
    }

    return true;
  }
}
