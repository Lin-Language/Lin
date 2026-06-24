'use strict';
// nbody.js — floating-point numerics (Computer Language Benchmarks Game "n-body").
// Symplectic-integrator simulation of the Jovian planets + Sun over N timesteps.
// Bodies are 7 parallel Float64 arrays; every product is bound to a local BEFORE
// any add/subtract so the op sequence is bit-identical to the other languages.
// Prints exactly one stdout line "RESULT=<int>".
//
// RESULT = Math.trunc(energy * 1e9) (truncate toward zero).
// Parameters (identical across all languages): N=5000000, dt=0.01.
const N = 5000000;
const PI = 3.141592653589793;
const SOLAR_MASS = 4.0 * PI * PI;
const DAYS_PER_YEAR = 365.24;

const x = [0.0, 4.84143144246472090, 8.34336671824457987, 12.8943695621391310, 15.3796971148509165];
const y = [0.0, -1.16032004402742839, 4.12479856412430479, -15.1111514016986312, -25.9193146099879641];
const z = [0.0, -0.103622044471123109, -0.403523417114321381, -0.223307578892655734, 0.179258772950371181];
const vx = [0.0, 0.00166007664274403694 * DAYS_PER_YEAR, -0.00276742510726862411 * DAYS_PER_YEAR, 0.00296460137564761618 * DAYS_PER_YEAR, 0.00268067772490389322 * DAYS_PER_YEAR];
const vy = [0.0, 0.00769901118419740425 * DAYS_PER_YEAR, 0.00499852801234917238 * DAYS_PER_YEAR, 0.00237847173959480950 * DAYS_PER_YEAR, 0.00162824170038242295 * DAYS_PER_YEAR];
const vz = [0.0, -0.0000690460016972063023 * DAYS_PER_YEAR, 0.0000230417297573763929 * DAYS_PER_YEAR, -0.0000296589568540237556 * DAYS_PER_YEAR, -0.0000951592254519715870 * DAYS_PER_YEAR];
const mass = [SOLAR_MASS, 0.000954791938424326609 * SOLAR_MASS, 0.000285885980666130812 * SOLAR_MASS, 0.0000436624404335156298 * SOLAR_MASS, 0.000151394322145122131 * SOLAR_MASS];

function offsetMomentum() {
  let px = 0.0, py = 0.0, pz = 0.0;
  for (let i = 0; i < 5; i++) {
    const mvx = vx[i] * mass[i];
    const mvy = vy[i] * mass[i];
    const mvz = vz[i] * mass[i];
    px += mvx;
    py += mvy;
    pz += mvz;
  }
  vx[0] = -px / SOLAR_MASS;
  vy[0] = -py / SOLAR_MASS;
  vz[0] = -pz / SOLAR_MASS;
}

function advance(dt) {
  for (let i = 0; i < 5; i++) {
    for (let j = i + 1; j < 5; j++) {
      const dx = x[i] - x[j];
      const dy = y[i] - y[j];
      const dz = z[i] - z[j];
      const sx = dx * dx;
      const sy = dy * dy;
      const sz = dz * dz;
      const d2 = sx + sy + sz;
      const dist = Math.sqrt(d2);
      const d2dist = d2 * dist;
      const mag = dt / d2dist;
      const mj = mass[j] * mag;
      const dxj = dx * mj;
      const dyj = dy * mj;
      const dzj = dz * mj;
      vx[i] -= dxj;
      vy[i] -= dyj;
      vz[i] -= dzj;
      const mi = mass[i] * mag;
      const dxi = dx * mi;
      const dyi = dy * mi;
      const dzi = dz * mi;
      vx[j] += dxi;
      vy[j] += dyi;
      vz[j] += dzi;
    }
  }
  for (let k = 0; k < 5; k++) {
    const px = dt * vx[k];
    const py = dt * vy[k];
    const pz = dt * vz[k];
    x[k] += px;
    y[k] += py;
    z[k] += pz;
  }
}

function energy() {
  let e = 0.0;
  for (let i = 0; i < 5; i++) {
    const sx = vx[i] * vx[i];
    const sy = vy[i] * vy[i];
    const sz = vz[i] * vz[i];
    const v2 = sx + sy + sz;
    const half = 0.5 * mass[i];
    const ke = half * v2;
    e += ke;
    for (let j = i + 1; j < 5; j++) {
      const dx = x[i] - x[j];
      const dy = y[i] - y[j];
      const dz = z[i] - z[j];
      const sxx = dx * dx;
      const syy = dy * dy;
      const szz = dz * dz;
      const d2 = sxx + syy + szz;
      const dist = Math.sqrt(d2);
      const mm = mass[i] * mass[j];
      const pe = mm / dist;
      e -= pe;
    }
  }
  return e;
}

function main() {
  offsetMomentum();
  for (let s = 0; s < N; s++) advance(0.01);
  const e1 = energy();
  const result = Math.trunc(e1 * 1000000000.0);
  console.log(`RESULT=${result}`);
}

main();
