// async_io.rs — I/O-bound concurrency with a dependency-free fixed pool of 50
// worker threads pulling 200 jobs off a shared queue; each sleeps 50ms then
// returns i*2+1. (A thread-pool-of-sleepers rather than an event loop — honest
// and documented; no tokio so the build stays a single `rustc -O`.) Prints
// "RESULT=<int>".
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

const TASKS: usize = 200;
const SLEEP_MS: u64 = 50;
const CONCURRENCY: usize = 50;

fn main() {
    let jobs: Arc<Mutex<VecDeque<usize>>> =
        Arc::new(Mutex::new((0..TASKS).collect()));
    let sum = Arc::new(Mutex::new(0i64));

    let mut workers = Vec::with_capacity(CONCURRENCY);
    for _ in 0..CONCURRENCY {
        let jobs = Arc::clone(&jobs);
        let sum = Arc::clone(&sum);
        workers.push(thread::spawn(move || loop {
            let job = {
                let mut q = jobs.lock().unwrap();
                q.pop_front()
            };
            match job {
                Some(i) => {
                    thread::sleep(Duration::from_millis(SLEEP_MS));
                    let v = (i as i64) * 2 + 1;
                    *sum.lock().unwrap() += v;
                }
                None => break,
            }
        }));
    }

    for w in workers {
        w.join().unwrap();
    }

    let total = *sum.lock().unwrap();
    println!("RESULT={}", total);
}
