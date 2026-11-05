//! A small deterministic random number generator (xorshift64*), so a run can be repeated exactly.

#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        // never zero, and nearby seeds do not give nearby streams
        let mut r = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        for _ in 0..4 {
            r.next_u64();
        }
        r
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard normal (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-300);
        let u2 = self.uniform();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    /// Uniform integer in [0, n).
    pub fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next_u64() % n as u64) as usize
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_numbers_and_different_seeds_differ() {
        let (mut a, mut b, mut c) = (Rng::new(7), Rng::new(7), Rng::new(8));
        let va: Vec<u64> = (0..5).map(|_| a.next_u64()).collect();
        let vb: Vec<u64> = (0..5).map(|_| b.next_u64()).collect();
        let vc: Vec<u64> = (0..5).map(|_| c.next_u64()).collect();
        assert_eq!(va, vb);
        assert_ne!(va, vc);
    }

    #[test]
    fn uniform_and_normal_look_like_themselves() {
        let mut r = Rng::new(1);
        let u: Vec<f64> = (0..20000).map(|_| r.uniform()).collect();
        assert!(u.iter().all(|x| (0.0..1.0).contains(x)));
        let mean = u.iter().sum::<f64>() / u.len() as f64;
        assert!((mean - 0.5).abs() < 0.01, "{mean}");
        let n: Vec<f64> = (0..20000).map(|_| r.normal()).collect();
        let m = n.iter().sum::<f64>() / n.len() as f64;
        let var = n.iter().map(|x| (x - m).powi(2)).sum::<f64>() / n.len() as f64;
        assert!(
            m.abs() < 0.03 && (var - 1.0).abs() < 0.05,
            "mean {m} var {var}"
        );
        assert!((0..1000).all(|_| r.below(7) < 7));
        assert_eq!(r.below(0), 0);
    }
}
