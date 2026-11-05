//! Looking inside: projections of the embedding space to 3D, attention maps, where the parameters
//! are, and the numbers behind "decay" in a Transformer.

use crate::model::Gpt;

/// Principal components: the three directions along which the rows differ most, found by power
/// iteration on the covariance matrix (deterministic). Returns one `[x, y, z]` per row, centred.
pub fn pca3(rows: &[Vec<f64>]) -> Vec<[f64; 3]> {
    let n = rows.len();
    let d = rows.first().map(|r| r.len()).unwrap_or(0);
    if n == 0 || d == 0 {
        return vec![];
    }
    let mean: Vec<f64> = (0..d)
        .map(|j| rows.iter().map(|r| r[j]).sum::<f64>() / n as f64)
        .collect();
    let x: Vec<Vec<f64>> = rows
        .iter()
        .map(|r| (0..d).map(|j| r[j] - mean[j]).collect())
        .collect();
    let mut cov = vec![0.0; d * d];
    for r in &x {
        for i in 0..d {
            if r[i] == 0.0 {
                continue;
            }
            for j in 0..d {
                cov[i * d + j] += r[i] * r[j];
            }
        }
    }
    cov.iter_mut().for_each(|c| *c /= n as f64);
    let mut axes: Vec<Vec<f64>> = vec![];
    for k in 0..3.min(d) {
        let mut v: Vec<f64> = (0..d)
            .map(|i| 1.0 / (i as f64 + 1.0) + k as f64 * 0.1 * ((i * 7 + k) % 5) as f64)
            .collect();
        for _ in 0..200 {
            let mut w = vec![0.0; d];
            for i in 0..d {
                for j in 0..d {
                    w[i] += cov[i * d + j] * v[j];
                }
            }
            for a in &axes {
                let dot: f64 = (0..d).map(|i| w[i] * a[i]).sum();
                (0..d).for_each(|i| w[i] -= dot * a[i]);
            }
            let norm = w.iter().map(|x| x * x).sum::<f64>().sqrt();
            if norm < 1e-12 {
                // nothing is left to vary along this direction (the cloud is flatter than 3D)
                v = vec![0.0; d];
                break;
            }
            v = w.iter().map(|x| x / norm).collect();
        }
        // a stable sign, so the picture does not flip between runs
        let big = v
            .iter()
            .cloned()
            .fold(0.0f64, |a, b| if b.abs() > a.abs() { b } else { a });
        if big < 0.0 {
            v.iter_mut().for_each(|x| *x = -*x);
        }
        axes.push(v);
    }
    x.iter()
        .map(|r| {
            let mut p = [0.0; 3];
            for (k, a) in axes.iter().enumerate() {
                p[k] = (0..d).map(|i| r[i] * a[i]).sum();
            }
            p
        })
        .collect()
}

pub fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let (na, nb) = (
        a.iter().map(|x| x * x).sum::<f64>().sqrt(),
        b.iter().map(|x| x * x).sum::<f64>().sqrt(),
    );
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na * nb)
    }
}

/// The `k` tokens whose embeddings point most nearly the same way as token `id`'s.
pub fn nearest_tokens(emb: &[Vec<f64>], id: usize, k: usize) -> Vec<(usize, f64)> {
    let mut s: Vec<(usize, f64)> = emb
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != id)
        .map(|(i, e)| (i, cosine_similarity(&emb[id], e)))
        .collect();
    s.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    s.truncate(k);
    s
}

/// The attention weights the model really used for `ids`: `[layer][head]` -> `T x T`, row-major.
pub fn attention_maps(model: &Gpt, ids: &[u32]) -> Vec<Vec<Vec<f64>>> {
    let ids = if ids.len() > model.config.ctx {
        &ids[ids.len() - model.config.ctx..]
    } else {
        ids
    };
    if ids.is_empty() {
        return vec![];
    }
    let t = ids.len();
    let fw = model.forward(ids);
    (0..model.config.layers)
        .map(|l| {
            let att = fw.attention(l);
            (0..model.config.heads)
                .map(|h| att[h * t * t..(h + 1) * t * t].to_vec())
                .collect()
        })
        .collect()
}

/// Where the parameters are: embeddings, attention, feed-forward, normalisation gains and biases.
pub fn param_groups(model: &Gpt) -> Vec<(String, usize)> {
    let c = &model.config;
    let (d, f, v, t, l) = (c.dim, c.ff(), c.vocab, c.ctx, c.layers);
    vec![
        ("token embeddings".to_string(), v * d),
        ("position embeddings".to_string(), t * d),
        ("attention (Q, K, V, output)".to_string(), l * 4 * d * d),
        ("feed-forward".to_string(), l * (d * f + f + f * d + d)),
        ("normalisation gains".to_string(), l * 2 * d + d),
    ]
}

/// The rotation speed of each pair of dimensions in rotary position embeddings (RoPE), which
/// DECAYS geometrically: the first pairs turn fast (they tell neighbouring tokens apart), the last
/// turn slowly (they tell far-apart tokens apart). `theta_i = base^(-2i/dim)`.
pub fn rope_frequencies(head_dim: usize, base: f64) -> Vec<f64> {
    (0..head_dim / 2)
        .map(|i| base.powf(-2.0 * i as f64 / head_dim as f64))
        .collect()
}

/// Shannon entropy of a probability distribution, in bits: how undecided the model is.
pub fn entropy_bits(p: &[f64]) -> f64 {
    -p.iter()
        .filter(|&&x| x > 0.0)
        .map(|&x| x * x.log2())
        .sum::<f64>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Config;

    #[test]
    fn pca_finds_the_directions_a_cloud_is_stretched_along() {
        // points spread widely along (1,1,0,0), a little along (0,0,1,0), none elsewhere
        let rows: Vec<Vec<f64>> = (0..40)
            .map(|i| {
                let a = (i as f64 - 20.0) * 1.0;
                let b = ((i * 7 % 11) as f64 - 5.0) * 0.1;
                vec![a, a, b, 0.0]
            })
            .collect();
        let p = pca3(&rows);
        assert_eq!(p.len(), 40);
        let var = |k: usize| p.iter().map(|q| q[k] * q[k]).sum::<f64>() / p.len() as f64;
        assert!(var(0) > 100.0 * var(1), "{} vs {}", var(0), var(1));
        assert!(
            var(1) > 1e-4 && var(2) < 1e-9,
            "the third direction is empty: {}",
            var(2)
        );
        assert_eq!(pca3(&rows), p, "deterministic");
        assert!(pca3(&[]).is_empty());
        assert_eq!(pca3(&[vec![1.0, 2.0]]).len(), 1);
    }

    #[test]
    fn similar_directions_are_close_and_opposite_ones_are_not() {
        assert!((cosine_similarity(&[1.0, 0.0], &[2.0, 0.0]) - 1.0).abs() < 1e-12);
        assert!((cosine_similarity(&[1.0, 0.0], &[-1.0, 0.0]) + 1.0).abs() < 1e-12);
        assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        let emb = vec![
            vec![1.0, 0.0],
            vec![0.9, 0.1],
            vec![-1.0, 0.0],
            vec![0.0, 1.0],
        ];
        let n = nearest_tokens(&emb, 0, 2);
        assert_eq!(n[0].0, 1);
        assert_eq!(n[1].0, 3);
    }

    #[test]
    fn attention_maps_have_the_right_shape_and_the_parameter_groups_add_up() {
        let m = Gpt::new(
            Config {
                vocab: 9,
                dim: 8,
                heads: 2,
                layers: 2,
                ctx: 6,
                ff_mult: 2,
            },
            1,
        )
        .unwrap();
        let maps = attention_maps(&m, &[1, 2, 3, 4]);
        assert_eq!((maps.len(), maps[0].len(), maps[0][0].len()), (2, 2, 16));
        assert_eq!(
            param_groups(&m).iter().map(|g| g.1).sum::<usize>(),
            m.n_params()
        );
        assert!(attention_maps(&m, &[]).is_empty());
    }

    #[test]
    fn rope_speeds_fall_geometrically_and_entropy_measures_indecision() {
        let f = rope_frequencies(8, 10000.0);
        assert_eq!(f.len(), 4);
        assert!((f[0] - 1.0).abs() < 1e-12);
        assert!(f.windows(2).all(|w| w[1] < w[0]));
        let r = f[1] / f[0];
        assert!(
            ((f[2] / f[1]) - r).abs() < 1e-12,
            "the same ratio each time: geometric decay"
        );
        assert!((entropy_bits(&[0.5, 0.5]) - 1.0).abs() < 1e-12);
        assert_eq!(entropy_bits(&[1.0, 0.0]), 0.0);
        assert!((entropy_bits(&[0.25; 4]) - 2.0).abs() < 1e-12);
    }
}
