// nbody.go — floating-point numerics (Computer Language Benchmarks Game "n-body").
// Symplectic-integrator simulation of the Jovian planets + Sun over N timesteps.
// Bodies are 7 parallel float64 arrays; every product is bound to a local BEFORE
// any add/subtract so the op sequence is bit-identical to the other languages
// (Go fuses x*y+z into an FMA only within a single expression — temps prevent it).
// The scaled initial state is built at runtime in main() so the constant math is
// ordinary runtime float64, matching the other languages. Prints "RESULT=<int>".
//
// RESULT = int64(energy * 1e9) (truncate toward zero).
// Parameters (identical across all languages): N=5000000, dt=0.01.
package main

import (
	"fmt"
	"math"
)

const n = 5000000

var x, y, z, vx, vy, vz, mass [5]float64

func offsetMomentum(solarMass float64) {
	px := 0.0
	py := 0.0
	pz := 0.0
	for i := 0; i < 5; i++ {
		mvx := vx[i] * mass[i]
		mvy := vy[i] * mass[i]
		mvz := vz[i] * mass[i]
		px += mvx
		py += mvy
		pz += mvz
	}
	vx[0] = -px / solarMass
	vy[0] = -py / solarMass
	vz[0] = -pz / solarMass
}

func advance(dt float64) {
	for i := 0; i < 5; i++ {
		for j := i + 1; j < 5; j++ {
			dx := x[i] - x[j]
			dy := y[i] - y[j]
			dz := z[i] - z[j]
			sx := dx * dx
			sy := dy * dy
			sz := dz * dz
			d2 := sx + sy + sz
			dist := math.Sqrt(d2)
			d2dist := d2 * dist
			mag := dt / d2dist
			mj := mass[j] * mag
			dxj := dx * mj
			dyj := dy * mj
			dzj := dz * mj
			vx[i] -= dxj
			vy[i] -= dyj
			vz[i] -= dzj
			mi := mass[i] * mag
			dxi := dx * mi
			dyi := dy * mi
			dzi := dz * mi
			vx[j] += dxi
			vy[j] += dyi
			vz[j] += dzi
		}
	}
	for k := 0; k < 5; k++ {
		px := dt * vx[k]
		py := dt * vy[k]
		pz := dt * vz[k]
		x[k] += px
		y[k] += py
		z[k] += pz
	}
}

func energy() float64 {
	e := 0.0
	for i := 0; i < 5; i++ {
		sx := vx[i] * vx[i]
		sy := vy[i] * vy[i]
		sz := vz[i] * vz[i]
		v2 := sx + sy + sz
		half := 0.5 * mass[i]
		ke := half * v2
		e += ke
		for j := i + 1; j < 5; j++ {
			dx := x[i] - x[j]
			dy := y[i] - y[j]
			dz := z[i] - z[j]
			sxx := dx * dx
			syy := dy * dy
			szz := dz * dz
			d2 := sxx + syy + szz
			dist := math.Sqrt(d2)
			mm := mass[i] * mass[j]
			pe := mm / dist
			e -= pe
		}
	}
	return e
}

func main() {
	pi := 3.141592653589793
	solarMass := 4.0 * pi * pi
	daysPerYear := 365.24

	x = [5]float64{0.0, 4.84143144246472090, 8.34336671824457987, 12.8943695621391310, 15.3796971148509165}
	y = [5]float64{0.0, -1.16032004402742839, 4.12479856412430479, -15.1111514016986312, -25.9193146099879641}
	z = [5]float64{0.0, -0.103622044471123109, -0.403523417114321381, -0.223307578892655734, 0.179258772950371181}
	vx = [5]float64{0.0, 0.00166007664274403694 * daysPerYear, -0.00276742510726862411 * daysPerYear, 0.00296460137564761618 * daysPerYear, 0.00268067772490389322 * daysPerYear}
	vy = [5]float64{0.0, 0.00769901118419740425 * daysPerYear, 0.00499852801234917238 * daysPerYear, 0.00237847173959480950 * daysPerYear, 0.00162824170038242295 * daysPerYear}
	vz = [5]float64{0.0, -0.0000690460016972063023 * daysPerYear, 0.0000230417297573763929 * daysPerYear, -0.0000296589568540237556 * daysPerYear, -0.0000951592254519715870 * daysPerYear}
	mass = [5]float64{solarMass, 0.000954791938424326609 * solarMass, 0.000285885980666130812 * solarMass, 0.0000436624404335156298 * solarMass, 0.000151394322145122131 * solarMass}

	offsetMomentum(solarMass)
	for s := 0; s < n; s++ {
		advance(0.01)
	}
	e1 := energy()
	result := int64(e1 * 1000000000.0)
	fmt.Printf("RESULT=%d\n", result)
}
