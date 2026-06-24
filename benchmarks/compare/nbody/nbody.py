# nbody.py — floating-point numerics (Computer Language Benchmarks Game "n-body").
# Symplectic-integrator simulation of the Jovian planets + Sun over N timesteps.
# Bodies are 7 parallel float lists; every product is bound to a local BEFORE any
# add/subtract so the op sequence is bit-identical to the other languages.
# Prints exactly one stdout line "RESULT=<int>".
#
# RESULT = int(energy * 1e9) (truncate toward zero).
# Parameters (identical across all languages): N=5000000, dt=0.01.
from math import sqrt

N = 5000000
PI = 3.141592653589793
SOLAR_MASS = 4.0 * PI * PI
DAYS_PER_YEAR = 365.24

x = [0.0, 4.84143144246472090, 8.34336671824457987, 12.8943695621391310, 15.3796971148509165]
y = [0.0, -1.16032004402742839, 4.12479856412430479, -15.1111514016986312, -25.9193146099879641]
z = [0.0, -0.103622044471123109, -0.403523417114321381, -0.223307578892655734, 0.179258772950371181]
vx = [0.0, 0.00166007664274403694 * DAYS_PER_YEAR, -0.00276742510726862411 * DAYS_PER_YEAR, 0.00296460137564761618 * DAYS_PER_YEAR, 0.00268067772490389322 * DAYS_PER_YEAR]
vy = [0.0, 0.00769901118419740425 * DAYS_PER_YEAR, 0.00499852801234917238 * DAYS_PER_YEAR, 0.00237847173959480950 * DAYS_PER_YEAR, 0.00162824170038242295 * DAYS_PER_YEAR]
vz = [0.0, -0.0000690460016972063023 * DAYS_PER_YEAR, 0.0000230417297573763929 * DAYS_PER_YEAR, -0.0000296589568540237556 * DAYS_PER_YEAR, -0.0000951592254519715870 * DAYS_PER_YEAR]
mass = [SOLAR_MASS, 0.000954791938424326609 * SOLAR_MASS, 0.000285885980666130812 * SOLAR_MASS, 0.0000436624404335156298 * SOLAR_MASS, 0.000151394322145122131 * SOLAR_MASS]


def offset_momentum():
    px = 0.0
    py = 0.0
    pz = 0.0
    for i in range(5):
        mvx = vx[i] * mass[i]
        mvy = vy[i] * mass[i]
        mvz = vz[i] * mass[i]
        px += mvx
        py += mvy
        pz += mvz
    vx[0] = -px / SOLAR_MASS
    vy[0] = -py / SOLAR_MASS
    vz[0] = -pz / SOLAR_MASS


def advance(dt):
    for i in range(5):
        for j in range(i + 1, 5):
            dx = x[i] - x[j]
            dy = y[i] - y[j]
            dz = z[i] - z[j]
            sx = dx * dx
            sy = dy * dy
            sz = dz * dz
            d2 = sx + sy + sz
            dist = sqrt(d2)
            d2dist = d2 * dist
            mag = dt / d2dist
            mj = mass[j] * mag
            dxj = dx * mj
            dyj = dy * mj
            dzj = dz * mj
            vx[i] -= dxj
            vy[i] -= dyj
            vz[i] -= dzj
            mi = mass[i] * mag
            dxi = dx * mi
            dyi = dy * mi
            dzi = dz * mi
            vx[j] += dxi
            vy[j] += dyi
            vz[j] += dzi
    for k in range(5):
        px = dt * vx[k]
        py = dt * vy[k]
        pz = dt * vz[k]
        x[k] += px
        y[k] += py
        z[k] += pz


def energy():
    e = 0.0
    for i in range(5):
        sx = vx[i] * vx[i]
        sy = vy[i] * vy[i]
        sz = vz[i] * vz[i]
        v2 = sx + sy + sz
        half = 0.5 * mass[i]
        ke = half * v2
        e += ke
        for j in range(i + 1, 5):
            dx = x[i] - x[j]
            dy = y[i] - y[j]
            dz = z[i] - z[j]
            sxx = dx * dx
            syy = dy * dy
            szz = dz * dz
            d2 = sxx + syy + szz
            dist = sqrt(d2)
            mm = mass[i] * mass[j]
            pe = mm / dist
            e -= pe
    return e


def main():
    offset_momentum()
    for _ in range(N):
        advance(0.01)
    e1 = energy()
    result = int(e1 * 1000000000.0)
    print(f"RESULT={result}")


if __name__ == "__main__":
    main()
