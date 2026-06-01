// dijkstra.rs — linear-scan priority-queue Dijkstra (O(V^2)) over graph.txt.
//
// Reads the line-based graph (no JSON library, single `rustc -O` build) INSIDE
// the timed region. Prints exactly one stdout line "RESULT=<int>".
use std::collections::HashMap;
use std::fs;

const INF: i64 = 1_000_000_000;

fn main() {
    // graph.txt lives next to this source's data dir, relative to repo root
    // (the runner cd's to repo root before invoking the binary).
    let path = "benchmarks/compare/data/graph.txt";
    let text = fs::read_to_string(path).expect("read graph.txt");

    let mut lines = text.lines();
    let header: Vec<&str> = lines.next().unwrap().split_whitespace().collect();
    let _num_nodes: usize = header[0].parse().unwrap();
    let source = header[1].to_string();
    let target = header[2].to_string();

    // Intern node names to indices as we read; preserve first-seen order so the
    // node set matches graph.json's "nodes" array (n0..n{N-1} in order).
    let mut id_of: HashMap<String, usize> = HashMap::new();
    let mut adj: Vec<Vec<(usize, i64)>> = Vec::new();

    let intern = |name: &str, id_of: &mut HashMap<String, usize>, adj: &mut Vec<Vec<(usize, i64)>>| -> usize {
        if let Some(&id) = id_of.get(name) {
            id
        } else {
            let id = adj.len();
            id_of.insert(name.to_string(), id);
            adj.push(Vec::new());
            id
        }
    };

    let src_id = intern(&source, &mut id_of, &mut adj);
    let tgt_id = intern(&target, &mut id_of, &mut adj);

    for line in lines {
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        let from = intern(parts[0], &mut id_of, &mut adj);
        let to = intern(parts[1], &mut id_of, &mut adj);
        let w: i64 = parts[2].parse().unwrap();
        adj[from].push((to, w));
    }

    let n = adj.len();
    let mut dist = vec![INF; n];
    dist[src_id] = 0;
    let mut visited = vec![false; n];

    // Linear-scan priority queue: a Vec of (node, dist) entries.
    let mut pq: Vec<(usize, i64)> = vec![(src_id, 0)];
    while !pq.is_empty() {
        let mut min_idx = 0;
        for i in 0..pq.len() {
            if pq[i].1 < pq[min_idx].1 {
                min_idx = i;
            }
        }
        let (u, _) = pq.swap_remove(min_idx);
        if visited[u] {
            continue;
        }
        visited[u] = true;
        for &(v, w) in &adj[u] {
            let nd = dist[u] + w;
            if nd < dist[v] {
                dist[v] = nd;
                pq.push((v, nd));
            }
        }
    }

    let mut total: i64 = 0;
    for &d in &dist {
        if d < INF {
            total += d;
        }
    }
    let result = dist[tgt_id] * 1_000_003 + (total % 1_000_000_000);
    eprintln!("dist[{}]={} sumFinite={}", target, dist[tgt_id], total);
    println!("RESULT={}", result);
}
