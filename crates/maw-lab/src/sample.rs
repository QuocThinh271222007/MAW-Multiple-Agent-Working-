//! Writing text with the model: pick the next token from its probabilities, append it, repeat.
//! Temperature and top-k decide how adventurous the choice is.

use crate::model::Gpt;
use crate::Rng;

#[derive(Clone, Copy, Debug)]
pub struct SampleOpts {
    /// 0 (or less) always takes the most likely token; higher is more random.
    pub temperature: f64,
    /// Only the `top_k` most likely tokens may be chosen (0 = all).
    pub top_k: usize,
    pub seed: u64,
}

impl Default for SampleOpts {
    fn default() -> Self {
        SampleOpts {
            temperature: 0.8,
            top_k: 0,
            seed: 1,
        }
    }
}

/// What happened at one step of generation.
#[derive(Clone, Debug)]
pub struct StepInfo {
    pub chosen: u32,
    /// The probability the model itself gave the chosen token (before temperature and top-k).
    pub prob: f64,
    /// The five most likely tokens by the model's own probabilities.
    pub top: Vec<(u32, f64)>,
}

pub struct Generated {
    pub ids: Vec<u32>,
    pub steps: Vec<StepInfo>,
}

/// The distribution tokens are really drawn from after temperature and top-k.
pub fn shape_distribution(probs: &[f64], temperature: f64, top_k: usize) -> Vec<f64> {
    let n = probs.len();
    if temperature <= 0.0 {
        let best = probs
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.partial_cmp(b.1).unwrap())
            .map(|x| x.0)
            .unwrap_or(0);
        let mut out = vec![0.0; n];
        out[best] = 1.0;
        return out;
    }
    // softmax(logits / T) is proportional to p^(1/T)
    let mut w: Vec<f64> = probs
        .iter()
        .map(|p| p.max(1e-300).powf(1.0 / temperature))
        .collect();
    if top_k > 0 && top_k < n {
        let mut sorted = w.clone();
        sorted.sort_by(|a, b| b.partial_cmp(a).unwrap());
        let cut = sorted[top_k - 1];
        let mut kept = 0;
        for x in w.iter_mut() {
            if *x < cut || kept >= top_k {
                *x = 0.0;
            } else {
                kept += 1;
            }
        }
    }
    let s: f64 = w.iter().sum();
    w.iter_mut().for_each(|x| *x /= s);
    w
}

pub fn generate(model: &Gpt, prompt: &[u32], n: usize, opts: &SampleOpts) -> Generated {
    let mut rng = Rng::new(opts.seed);
    let mut ids = prompt.to_vec();
    let mut steps = vec![];
    for _ in 0..n {
        let p = model.next_token_probs(&ids);
        let shaped = shape_distribution(&p, opts.temperature, opts.top_k);
        let r = rng.uniform();
        let mut acc = 0.0;
        let mut chosen = shaped.len() - 1;
        for (i, &w) in shaped.iter().enumerate() {
            acc += w;
            if r < acc && w > 0.0 {
                chosen = i;
                break;
            }
        }
        let mut top: Vec<(u32, f64)> = p.iter().enumerate().map(|(i, &q)| (i as u32, q)).collect();
        top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        top.truncate(5);
        steps.push(StepInfo {
            chosen: chosen as u32,
            prob: p[chosen],
            top,
        });
        ids.push(chosen as u32);
    }
    Generated { ids, steps }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Config;

    #[test]
    fn temperature_sharpens_or_flattens_and_top_k_cuts_the_tail() {
        let p = [0.5, 0.3, 0.15, 0.05];
        let cold = shape_distribution(&p, 0.2, 0);
        let warm = shape_distribution(&p, 2.0, 0);
        assert!(cold[0] > 0.9, "{cold:?}");
        assert!(warm[3] > 0.1 && warm[0] < 0.45, "{warm:?}");
        let k2 = shape_distribution(&p, 1.0, 2);
        assert_eq!(k2[2], 0.0);
        assert_eq!(k2[3], 0.0);
        assert!((k2.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert_eq!(
            shape_distribution(&p, 0.0, 0),
            vec![1.0, 0.0, 0.0, 0.0],
            "greedy"
        );
        for d in [&cold, &warm, &k2] {
            assert!((d.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        }
    }

    #[test]
    fn generation_is_reproducible_and_reports_each_choice() {
        let m = Gpt::new(
            Config {
                vocab: 6,
                dim: 8,
                heads: 2,
                layers: 1,
                ctx: 8,
                ff_mult: 2,
            },
            2,
        )
        .unwrap();
        let o = SampleOpts {
            temperature: 1.0,
            top_k: 0,
            seed: 4,
        };
        let a = generate(&m, &[1, 2], 12, &o);
        let b = generate(&m, &[1, 2], 12, &o);
        assert_eq!(a.ids, b.ids);
        assert_eq!(a.ids.len(), 14);
        assert_eq!(a.steps.len(), 12);
        assert!(a
            .steps
            .iter()
            .all(|s| s.top.len() == 5 && s.prob > 0.0 && s.prob <= 1.0));
        // longer than the context window still works: only the last tokens are read
        assert_eq!(generate(&m, &[1; 20], 3, &o).ids.len(), 23);
        let g = generate(
            &m,
            &[1],
            6,
            &SampleOpts {
                temperature: 0.0,
                top_k: 0,
                seed: 1,
            },
        );
        let again = generate(
            &m,
            &[1],
            6,
            &SampleOpts {
                temperature: 0.0,
                top_k: 0,
                seed: 99,
            },
        );
        assert_eq!(g.ids, again.ids, "greedy does not depend on the seed");
    }
}
