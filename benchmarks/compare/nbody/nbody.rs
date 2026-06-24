// nbody.rs — floating-point numerics (Computer Language Benchmarks Game "n-body").
// Symplectic-integrator simulation of the Jovian planets + Sun over N timesteps.
// Bodies are 7 parallel [f64;5] arrays; every product is bound to a local BEFORE
// any add/subtract so the op sequence is bit-identical to the other languages
// (rustc/LLVM does not contract to FMA without fast-math; temps make it explicit).
// The scaled initial state is built at runtime in main() so the constant math is
// ordinary runtime f64, matching the other languages. Prints "RESULT=<int>".
//
// RESULT = (energy * 1e9) as i64 (truncate toward zero).
// Parameters (identical across all languages): N=5000000, dt=0.01.
const N: usize = 5000000;

fn offset_momentum(vx: &mut [f64; 5], vy: &mut [f64; 5], vz: &mut [f64; 5], mass: &[f64; 5], solar_mass: f64) {
    let mut px = 0.0;
    let mut py = 0.0;
    let mut pz = 0.0;
    for i in 0..5 {
        let mvx = vx[i] * mass[i];
        let mvy = vy[i] * mass[i];
        let mvz = vz[i] * mass[i];
        px += mvx;
        py += mvy;
        pz += mvz;
    }
    vx[0] = -px / solar_mass;
    vy[0] = -py / solar_mass;
    vz[0] = -pz / solar_mass;
}

fn advance(
    x: &mut [f64; 5], y: &mut [f64; 5], z: &mut [f64; 5],
    vx: &mut [f64; 5], vy: &mut [f64; 5], vz: &mut [f64; 5],
    mass: &[f64; 5], dt: f64,
) {
    for i in 0..5 {
        for j in (i + 1)..5 {
            let dx = x[i] - x[j];
            let dy = y[i] - y[j];
            let dz = z[i] - z[j];
            let sx = dx * dx;
            let sy = dy * dy;
            let sz = dz * dz;
            let d2 = sx + sy + sz;
            let dist = d2.sqrt();
            let d2dist = d2 * dist;
            let mag = dt / d2dist;
            let mj = mass[j] * mag;
            let dxj = dx * mj;
            let dyj = dy * mj;
            let dzj = dz * mj;
            vx[i] -= dxj;
            vy[i] -= dyj;
            vz[i] -= dzj;
            let mi = mass[i] * mag;
            let dxi = dx * mi;
            let dyi = dy * mi;
            let dzi = dz * mi;
            vx[j] += dxi;
            vy[j] += dyi;
            vz[j] += dzi;
        }
    }
    for k in 0..5 {
        let px = dt * vx[k];
        let py = dt * vy[k];
        let pz = dt * vz[k];
        x[k] += px;
        y[k] += py;
        z[k] += pz;
    }
}

fn energy(
    x: &[f64; 5], y: &[f64; 5], z: &[f64; 5],
    vx: &[f64; 5], vy: &[f64; 5], vz: &[f64; 5],
    mass: &[f64; 5],
) -> f64 {
    let mut e = 0.0;
    for i in 0..5 {
        let sx = vx[i] * vx[i];
        let sy = vy[i] * vy[i];
        let sz = vz[i] * vz[i];
        let v2 = sx + sy + sz;
        let half = 0.5 * mass[i];
        let ke = half * v2;
        e += ke;
        for j in (i + 1)..5 {
            let dx = x[i] - x[j];
            let dy = y[i] - y[j];
            let dz = z[i] - z[j];
            let sxx = dx * dx;
            let syy = dy * dy;
            let szz = dz * dz;
            let d2 = sxx + syy + szz;
            let dist = d2.sqrt();
            let mm = mass[i] * mass[j];
            let pe = mm / dist;
            e -= pe;
        }
    }
    e
}

fn main() {
    let pi: f64 = 3.141592653589793;
    let solar_mass = 4.0 * pi * pi;
    let dpy = 365.24;

    let mut x: [f64; 5] = [0.0, 4.84143144246472090, 8.34336671824457987, 12.8943695621391310, 15.3796971148509165];
    let mut y: [f64; 5] = [0.0, -1.16032004402742839, 4.12479856412430479, -15.1111514016986312, -25.9193146099879641];
    let mut z: [f64; 5] = [0.0, -0.103622044471123109, -0.403523417114321381, -0.223307578892655734, 0.179258772950371181];
    let mut vx: [f64; 5] = [0.0, 0.00166007664274403694 * dpy, -0.00276742510726862411 * dpy, 0.00296460137564761618 * dpy, 0.00268067772490389322 * dpy];
    let mut vy: [f64; 5] = [0.0, 0.00769901118419740425 * dpy, 0.00499852801234917238 * dpy, 0.00237847173959480950 * dpy, 0.00162824170038242295 * dpy];
    let mut vz: [f64; 5] = [0.0, -0.0000690460016972063023 * dpy, 0.0000230417297573763929 * dpy, -0.0000296589568540237556 * dpy, -0.0000951592254519715870 * dpy];
    let mass: [f64; 5] = [solar_mass, 0.000954791938424326609 * solar_mass, 0.000285885980666130812 * solar_mass, 0.0000436624404335156298 * solar_mass, 0.000151394322145122131 * solar_mass];

    offset_momentum(&mut vx, &mut vy, &mut vz, &mass, solar_mass);
    for _ in 0..N {
        advance(&mut x, &mut y, &mut z, &mut vx, &mut vy, &mut vz, &mass, 0.01);
    }
    let e1 = energy(&x, &y, &z, &vx, &vy, &vz, &mass);
    let result = (e1 * 1000000000.0) as i64;
    println!("RESULT={}", result);
}
