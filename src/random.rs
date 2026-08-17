//! A small pseudo random generator, seeded by hand and reproducible forever.
//!
//! A generated dataset is only worth anything if it can be generated again:
//! a map which shows up a rendering bug has to still be the same map next
//! week, on another machine, after a `cargo update`. The generators of the
//! `rand` crate are explicitly allowed to change their output between
//! versions, so the few dozen lines it would save are paid for with exactly
//! the property this needs.
//!
//! So the algorithm is written out here and never changes: xoshiro256\*\*
//! over a state spread out of the seed by SplitMix64, both public domain.
//! Same seed, same numbers, same maps.

/// A stream of pseudo random numbers, from a seed.
///
/// Two generators made from the same seed give the same sequence, and
/// generators made from *neighbouring* seeds do not resemble each other: the
/// SplitMix64 pass over the seed is what buys that, and it is why a dataset
/// can seed its n-th map with `seed + n` and still get n unrelated maps.
pub struct Random {
    state: [u64; 4],
}

/// The odd constant SplitMix64 walks the seed by: the 64 bit golden ratio.
const GOLDEN: u64 = 0x9E37_79B9_7F4A_7C15;

impl Random {
    /// A generator for `seed`.
    pub fn from_seed(seed: u64) -> Random {
        // SplitMix64, whose whole job is turning a seed nobody chose with any
        // care -- 0, 1, 2 -- into 256 bits with no structure left in them.
        let mut z = seed;
        let mut next = || {
            z = z.wrapping_add(GOLDEN);
            let mut x = z;
            x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            x ^ (x >> 31)
        };
        Random {
            state: [next(), next(), next(), next()],
        }
    }

    /// The next 64 bits.
    pub fn next_u64(&mut self) -> u64 {
        // xoshiro256**, as its authors write it.
        let [s0, s1, s2, s3] = self.state;
        let result = s1.wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = s1 << 17;
        let mut state = [s0 ^ s3, s1 ^ s2, s2 ^ s0, s3 ^ s1];
        state[2] ^= t;
        state[3] = state[3].rotate_left(45);
        self.state = state;
        result
    }

    /// A number in `[0, 1)`.
    ///
    /// The 53 top bits of one draw, which is as much of one as a `f64` can
    /// hold without rounding up to 1.0.
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// A number in `[low, high)`.
    pub fn between(&mut self, low: f64, high: f64) -> f64 {
        low + self.unit() * (high - low)
    }

    /// A number in `[-magnitude, magnitude)`.
    pub fn symmetric(&mut self, magnitude: f64) -> f64 {
        self.between(-magnitude, magnitude)
    }

    /// A whole number in `[0, bound)`, or 0 where there is no such number.
    ///
    /// Lemire's multiply-and-shift rather than a remainder: the bias it
    /// leaves is one part in 2^64 per value, which nothing here can tell from
    /// none at all, and it costs a multiplication instead of a division.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound <= 1 {
            return 0;
        }
        ((self.next_u64() as u128 * bound as u128) >> 64) as usize
    }

    /// A whole number in `[low, high]`.
    pub fn in_range(&mut self, low: usize, high: usize) -> usize {
        if high <= low {
            return low;
        }
        low + self.below(high - low + 1)
    }

    /// True with the given probability.
    pub fn chance(&mut self, probability: f64) -> bool {
        self.unit() < probability
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_numbers() {
        let mut one = Random::from_seed(7);
        let mut other = Random::from_seed(7);
        for _ in 0..100 {
            assert_eq!(one.next_u64(), other.next_u64());
        }
    }

    #[test]
    fn neighbouring_seeds_do_not_resemble_each_other() {
        // What lets a dataset seed its n-th map with seed + n.
        let first: Vec<u64> = (0..8).map(|_| Random::from_seed(0).next_u64()).collect();
        let second: Vec<u64> = (0..8).map(|_| Random::from_seed(1).next_u64()).collect();
        assert_ne!(first, second);
    }

    #[test]
    fn a_unit_number_stays_inside_its_interval() {
        let mut random = Random::from_seed(1);
        for _ in 0..10_000 {
            let value = random.unit();
            assert!((0.0..1.0).contains(&value), "{value}");
        }
    }

    #[test]
    fn a_whole_number_stays_inside_its_bound() {
        let mut random = Random::from_seed(2);
        for _ in 0..10_000 {
            assert!(random.below(5) < 5);
            assert!((3..=7).contains(&random.in_range(3, 7)));
        }
        // Degenerate bounds give the only number there is.
        assert_eq!(random.below(0), 0);
        assert_eq!(random.below(1), 0);
        assert_eq!(random.in_range(4, 4), 4);
    }

    #[test]
    fn a_chance_comes_out_about_as_often_as_it_says() {
        let mut random = Random::from_seed(3);
        let taken = (0..10_000).filter(|_| random.chance(0.25)).count();
        assert!((2300..2700).contains(&taken), "{taken}");
    }
}
