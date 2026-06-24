// dijkstra.rs — linear-scan priority-queue Dijkstra (O(V^2)) over an in-code graph.
// Graph generated in memory by a portable deterministic generator (no file I/O).
// Prints exactly one stdout line "RESULT=<int>". Generator: Park-Miller MINSTD.
const N: usize = 30000;
const INF: i64 = 1000000000;

fn main() {
    let mut state: i64 = 1234;
    let mut nxt = || {
        state = (state * 16807) % 2147483647;
        state
    };

    let mut adj: Vec<Vec<(usize, i64)>> = vec![Vec::new(); N];
    for i in 0..N {
        for d in 1..=8 {
            let j = i + d;
            if j < N {
                let w = nxt() % 100 + 1;
                adj[i].push((j, w));
            }
        }
        if i + 1 < N {
            let r = nxt();
            if r % 10 < 3 {
                let span = (N - (i + 1)) as i64;
                let j = (i + 1) + (nxt() % span) as usize;
                let w = nxt() % 100 + 1;
                adj[i].push((j, w));
            }
        }
    }

    let mut dist = vec![INF; N];
    let mut visited = vec![false; N];
    dist[0] = 0;
    let capn = N * 9 + 1;
    let mut pqn = vec![0usize; capn];
    let mut pqd = vec![0i64; capn];
    let mut pql = 1usize;
    while pql > 0 {
        let mut mi = 0;
        for j in 1..pql {
            if pqd[j] < pqd[mi] {
                mi = j;
            }
        }
        let u = pqn[mi];
        let last = pql - 1;
        pqn[mi] = pqn[last];
        pqd[mi] = pqd[last];
        pql = last;
        if !visited[u] {
            visited[u] = true;
            let du = dist[u];
            for k in 0..adj[u].len() {
                let (v, w) = adj[u][k];
                let nd = du + w;
                if nd < dist[v] {
                    dist[v] = nd;
                    pqn[pql] = v;
                    pqd[pql] = nd;
                    pql += 1;
                }
            }
        }
    }
    let mut total: i64 = 0;
    for k in 0..N {
        if dist[k] < INF {
            total += dist[k];
        }
    }
    let chk = dist[N - 1] * 1000003 + (total % 1000000000);
    println!("RESULT={}", chk);
}
