//! Transfer-pattern result builders
//! (port of src/transfer-pattern/results/{GraphResults,StringResults}.ts).

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::rc::Rc;

use indexmap::{IndexMap, IndexSet};

use crate::gtfs::{StopId, Time, MAX_SAFE_INTEGER};
use crate::raptor::Interchange;
use crate::scan_results::{Connection, ConnectionIndex};

/// Extract the origin for a connection: transfer.origin, else trip.stopTimes[start].stop.
fn connection_origin(connection: &Connection) -> StopId {
    match connection {
        Connection::Transfer(t) => t.origin.clone(),
        Connection::Trip(trip, start, _) => trip.stop_times[*start].stop.clone(),
    }
}

// ---------------------------------------------------------------------------
// GraphResults — store transfer patterns as a DAG.
// ---------------------------------------------------------------------------

/// Graph node maintaining a reference to its parent. Identity is structural
/// (label + parent chain), matching the spec's `toEqual` comparison.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub label: StopId,
    pub parent: Option<Rc<TreeNode>>,
}

impl TreeNode {
    /// Walk the parent chain comparing labels to `path[i]` (port of `isSame`).
    fn is_same(path: &[StopId], mut node: Option<&Rc<TreeNode>>) -> bool {
        let mut i = 0;
        while let Some(n) = node {
            if path.get(i).map(|s| s.as_str()) != Some(n.label.as_str()) {
                return false;
            }
            node = n.parent.as_ref();
            i += 1;
        }
        true
    }
}

/// Leaf nodes indexed by label, in insertion order.
pub type TransferPatternGraph = IndexMap<StopId, Vec<Rc<TreeNode>>>;

#[derive(Default)]
pub struct GraphResults {
    results: RefCell<TransferPatternGraph>,
}

impl GraphResults {
    pub fn new() -> Self {
        GraphResults::default()
    }

    pub fn add(&self, k_connections: &ConnectionIndex) {
        for path in Self::get_paths(k_connections) {
            self.merge_path(&path);
        }
    }

    pub fn finalize(self) -> TransferPatternGraph {
        self.results.into_inner()
    }

    fn get_paths(k_connections: &ConnectionIndex) -> Vec<Vec<StopId>> {
        let mut results = Vec::new();
        for (destination, rounds) in k_connections {
            for &k in rounds.keys() {
                results.push(Self::get_path(k_connections, k, destination));
            }
        }
        results
    }

    fn get_path(k_connections: &ConnectionIndex, k: usize, final_destination: &str) -> Vec<StopId> {
        let mut path = vec![final_destination.to_string()];
        let mut destination = final_destination.to_string();
        let mut i = k;

        while i > 0 {
            let connection = &k_connections[&destination][&i];
            let origin = connection_origin(connection);
            path.push(origin.clone());
            destination = origin;
            i -= 1;
        }

        path
    }

    /// Merge path `[head, ...tail]` into the graph, returning the head node.
    fn merge_path(&self, path: &[StopId]) -> Rc<TreeNode> {
        let head = path[0].clone();
        let tail = &path[1..];

        // Look for an existing node whose parent chain matches `tail`.
        let existing = {
            let results = self.results.borrow();
            results.get(&head).and_then(|nodes| {
                nodes
                    .iter()
                    .find(|n| TreeNode::is_same(tail, n.parent.as_ref()))
                    .cloned()
            })
        };

        if let Some(node) = existing {
            return node;
        }

        let parent = if !tail.is_empty() {
            Some(self.merge_path(tail))
        } else {
            None
        };

        let node = Rc::new(TreeNode {
            label: head.clone(),
            parent,
        });

        self.results
            .borrow_mut()
            .entry(head)
            .or_default()
            .push(Rc::clone(&node));

        node
    }
}

// ---------------------------------------------------------------------------
// StringResults — store change points as pattern strings.
// ---------------------------------------------------------------------------

/// Transfer-pattern strings indexed by journey key. The inner set is insertion-ordered
/// to mirror the JS `Set` (which preserves insertion order) for `toEqual`.
pub type TransferPatternIndex = IndexMap<String, IndexSet<String>>;

pub struct StringResults {
    interchange: Interchange,
    results: TransferPatternIndex,
}

impl StringResults {
    pub fn new(interchange: Interchange) -> Self {
        StringResults {
            interchange,
            results: IndexMap::new(),
        }
    }

    pub fn add(&mut self, k_connections: &ConnectionIndex) -> Time {
        let mut next_departure_time = MAX_SAFE_INTEGER;

        for (destination, rounds) in k_connections {
            for &k in rounds.keys() {
                let (path, departure_time) = self.get_path(k_connections, k, destination);

                if !path.is_empty() {
                    let origin = &path[0];
                    let tail: Vec<String> = path[1..].to_vec();

                    let journey_key = if origin > destination {
                        format!("{destination}{origin}")
                    } else {
                        format!("{origin}{destination}")
                    };

                    let path_string = if origin > destination {
                        let mut rev = tail.clone();
                        rev.reverse();
                        rev.join(",")
                    } else {
                        tail.join(",")
                    };

                    self.results
                        .entry(journey_key)
                        .or_default()
                        .insert(path_string);

                    next_departure_time =
                        next_departure_time.min(departure_time.saturating_add(1));
                }
            }
        }

        next_departure_time
    }

    pub fn finalize(self) -> TransferPatternIndex {
        self.results
    }

    fn get_path(
        &self,
        k_connections: &ConnectionIndex,
        k: usize,
        final_destination: &str,
    ) -> (Vec<StopId>, Time) {
        let mut path: Vec<StopId> = Vec::new();
        let mut departure_time = MAX_SAFE_INTEGER;
        let mut destination = final_destination.to_string();
        let mut i = k;

        while i > 0 {
            let connection = &k_connections[&destination][&i];
            let origin = connection_origin(connection);

            departure_time = match connection {
                Connection::Transfer(t) => {
                    let inter = self.interchange.get(&t.destination).copied().unwrap_or(0);
                    departure_time - t.duration - inter
                }
                Connection::Trip(trip, start, _) => trip.stop_times[*start].departure_time,
            };

            path.insert(0, origin.clone());
            destination = origin;
            i -= 1;
        }

        (path, departure_time)
    }
}

/// Helper retained for parity with the JS `Set`-based comparisons in tests.
pub fn to_sorted_set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}
