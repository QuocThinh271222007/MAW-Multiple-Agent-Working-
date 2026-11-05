//! The Transformer, with back-propagation written out by hand.
//!
//! Architecture (a small GPT): token embedding + learned position embedding, then `layers` blocks of
//! `x = x + Attention(RMSNorm(x))` and `x = x + FeedForward(RMSNorm(x))`, a final RMSNorm, and the
//! output layer that shares its weights with the token embedding (so a token's embedding is both
//! how it is read and how it is predicted). Attention is multi-head and causal: a position can only
//! look at itself and what came before.
//!
//! All parameters live in ONE flat vector (`params`), described by [`Layout`]; gradients and the
//! optimizer's memory use the same layout. That keeps the code short and makes "which numbers are
//! the embeddings, which are layer 2's attention" a plain slice.

// Index loops mirror the maths (`for s in 0..=i`, `row[s]`); iterators would hide it.
#![allow(clippy::needless_range_loop)]

use crate::{LabError, Result, Rng};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Config {
    pub vocab: usize,
    /// Width of the vectors that flow through the model (the embedding size).
    pub dim: usize,
    pub heads: usize,
    pub layers: usize,
    /// Longest text (in tokens) the model can look at once.
    pub ctx: usize,
    /// The feed-forward layer is `dim * ff_mult` wide.
    pub ff_mult: usize,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        if self.vocab < 2
            || self.dim == 0
            || self.heads == 0
            || self.layers == 0
            || self.ctx < 2
            || self.ff_mult == 0
        {
            return Err(LabError::Invalid("every size of the model must be at least 1 (and the vocabulary and context at least 2)".into()));
        }
        if self.dim.checked_rem(self.heads) != Some(0) {
            return Err(LabError::Invalid(format!(
                "dim {} must be divisible by heads {}: each head gets dim/heads numbers",
                self.dim, self.heads
            )));
        }
        if self.dim > 512 || self.layers > 12 || self.ctx > 512 || self.vocab > 5000 {
            return Err(LabError::Invalid(
                "this is a model for learning: dim <= 512, layers <= 12, ctx <= 512, vocab <= 5000"
                    .into(),
            ));
        }
        Ok(())
    }

    pub fn head_dim(&self) -> usize {
        self.dim / self.heads
    }

    pub fn ff(&self) -> usize {
        self.dim * self.ff_mult
    }
}

/// Where every tensor starts in the flat parameter vector.
#[derive(Clone, Debug)]
pub struct Layout {
    pub tok_emb: usize,
    pub pos_emb: usize,
    pub layers: Vec<LayerOff>,
    pub gf: usize,
    pub total: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct LayerOff {
    pub g1: usize,
    pub wq: usize,
    pub wk: usize,
    pub wv: usize,
    pub wo: usize,
    pub g2: usize,
    pub w1: usize,
    pub b1: usize,
    pub w2: usize,
    pub b2: usize,
}

impl Layout {
    pub fn new(c: &Config) -> Layout {
        let (v, d, t, f) = (c.vocab, c.dim, c.ctx, c.ff());
        let mut at = 0usize;
        let mut take = |n: usize| {
            let o = at;
            at += n;
            o
        };
        let tok_emb = take(v * d);
        let pos_emb = take(t * d);
        let layers = (0..c.layers)
            .map(|_| LayerOff {
                g1: take(d),
                wq: take(d * d),
                wk: take(d * d),
                wv: take(d * d),
                wo: take(d * d),
                g2: take(d),
                w1: take(d * f),
                b1: take(f),
                w2: take(f * d),
                b2: take(d),
            })
            .collect();
        let gf = take(d);
        Layout {
            tok_emb,
            pos_emb,
            layers,
            gf,
            total: at,
        }
    }

    /// `true` for the parameters weight decay applies to: the matrices (not the gains and biases).
    pub fn decay_mask(&self, c: &Config) -> Vec<bool> {
        let (v, d, t, f) = (c.vocab, c.dim, c.ctx, c.ff());
        let mut m = vec![false; self.total];
        let mut set = |o: usize, n: usize| m[o..o + n].iter_mut().for_each(|x| *x = true);
        set(self.tok_emb, v * d);
        set(self.pos_emb, t * d);
        for l in &self.layers {
            for (o, n) in [
                (l.wq, d * d),
                (l.wk, d * d),
                (l.wv, d * d),
                (l.wo, d * d),
                (l.w1, d * f),
                (l.w2, f * d),
            ] {
                set(o, n);
            }
        }
        m
    }
}

#[derive(Clone, Debug)]
pub struct Gpt {
    pub config: Config,
    pub layout: Layout,
    pub params: Vec<f64>,
}

const EPS: f64 = 1e-5;

// ------------------------------------------------------------------ small linear algebra

/// `out[n x m] = a[n x k] * b[k x m]` (row-major, overwrites `out`).
fn matmul(a: &[f64], n: usize, k: usize, b: &[f64], m: usize, out: &mut [f64]) {
    out[..n * m].iter_mut().for_each(|x| *x = 0.0);
    for i in 0..n {
        for p in 0..k {
            let av = a[i * k + p];
            if av == 0.0 {
                continue;
            }
            let (brow, orow) = (&b[p * m..p * m + m], &mut out[i * m..i * m + m]);
            for j in 0..m {
                orow[j] += av * brow[j];
            }
        }
    }
}

/// `out[k x m] += a[n x k]^T * g[n x m]`.
fn acc_at_b(a: &[f64], n: usize, k: usize, g: &[f64], m: usize, out: &mut [f64]) {
    for i in 0..n {
        for p in 0..k {
            let av = a[i * k + p];
            if av == 0.0 {
                continue;
            }
            let (grow, orow) = (&g[i * m..i * m + m], &mut out[p * m..p * m + m]);
            for j in 0..m {
                orow[j] += av * grow[j];
            }
        }
    }
}

/// `out[n x k] += g[n x m] * b[k x m]^T`.
fn acc_g_bt(g: &[f64], n: usize, m: usize, b: &[f64], k: usize, out: &mut [f64]) {
    for i in 0..n {
        for p in 0..k {
            let mut s = 0.0;
            let (grow, brow) = (&g[i * m..i * m + m], &b[p * m..p * m + m]);
            for j in 0..m {
                s += grow[j] * brow[j];
            }
            out[i * k + p] += s;
        }
    }
}

/// `y = g * x / rms(x)` per row; remembers `1/rms` in `r` for the backward pass.
fn rmsnorm(x: &[f64], g: &[f64], t: usize, d: usize, out: &mut [f64], r: &mut [f64]) {
    for i in 0..t {
        let row = &x[i * d..i * d + d];
        let ms = row.iter().map(|v| v * v).sum::<f64>() / d as f64;
        let inv = 1.0 / (ms + EPS).sqrt();
        r[i] = inv;
        for j in 0..d {
            out[i * d + j] = g[j] * row[j] * inv;
        }
    }
}

/// Backward of [`rmsnorm`]: adds to `dx` and `dg`.
#[allow(clippy::too_many_arguments)]
fn rmsnorm_back(
    x: &[f64],
    g: &[f64],
    r: &[f64],
    dy: &[f64],
    t: usize,
    d: usize,
    dx: &mut [f64],
    dg: &mut [f64],
) {
    for i in 0..t {
        let (xr, dyr) = (&x[i * d..i * d + d], &dy[i * d..i * d + d]);
        let dot: f64 = (0..d).map(|j| g[j] * dyr[j] * xr[j]).sum();
        let r3 = r[i] * r[i] * r[i] / d as f64;
        for j in 0..d {
            dx[i * d + j] += r[i] * g[j] * dyr[j] - r3 * xr[j] * dot;
            dg[j] += dyr[j] * xr[j] * r[i];
        }
    }
}

fn gelu(x: f64) -> f64 {
    let c = (2.0 / std::f64::consts::PI).sqrt();
    0.5 * x * (1.0 + (c * (x + 0.044715 * x * x * x)).tanh())
}

fn gelu_grad(x: f64) -> f64 {
    let c = (2.0 / std::f64::consts::PI).sqrt();
    let u = c * (x + 0.044715 * x * x * x);
    let th = u.tanh();
    0.5 * (1.0 + th) + 0.5 * x * (1.0 - th * th) * c * (1.0 + 3.0 * 0.044715 * x * x)
}

// ------------------------------------------------------------------ the model

#[derive(Default)]
struct LayerCache {
    x_in: Vec<f64>,
    r1: Vec<f64>,
    n1: Vec<f64>,
    q: Vec<f64>,
    k: Vec<f64>,
    v: Vec<f64>,
    /// `[head][t][s]`, each row a probability distribution over the positions it may look at.
    att: Vec<f64>,
    ctx: Vec<f64>,
    x1: Vec<f64>,
    r2: Vec<f64>,
    n2: Vec<f64>,
    h1: Vec<f64>,
    a: Vec<f64>,
}

pub struct Forward {
    /// `T x vocab`: the score for every possible next token at every position.
    pub logits: Vec<f64>,
    layers: Vec<LayerCache>,
    xf: Vec<f64>,
    rf: Vec<f64>,
    nf: Vec<f64>,
}

impl Forward {
    /// Attention weights of one layer: `heads` blocks of `T x T`.
    pub fn attention(&self, layer: usize) -> &[f64] {
        &self.layers[layer].att
    }
}

impl Gpt {
    pub fn new(config: Config, seed: u64) -> Result<Gpt> {
        config.validate()?;
        let layout = Layout::new(&config);
        let mut rng = Rng::new(seed);
        let mut params = vec![0.0; layout.total];
        let (d, f) = (config.dim, config.ff());
        let res = 1.0 / (2.0 * config.layers as f64).sqrt();
        let mut fill = |o: usize, n: usize, std: f64| {
            for p in params[o..o + n].iter_mut() {
                *p = rng.normal() * std;
            }
        };
        fill(layout.tok_emb, config.vocab * d, 0.1);
        fill(layout.pos_emb, config.ctx * d, 0.1);
        for l in &layout.layers {
            let s = 1.0 / (d as f64).sqrt();
            fill(l.wq, d * d, s);
            fill(l.wk, d * d, s);
            fill(l.wv, d * d, s);
            fill(l.wo, d * d, s * res);
            fill(l.w1, d * f, s);
            fill(l.w2, f * d, (1.0 / (f as f64).sqrt()) * res);
        }
        // the gains start at 1 (norm does nothing yet), biases at 0
        for l in &layout.layers {
            params[l.g1..l.g1 + d].iter_mut().for_each(|x| *x = 1.0);
            params[l.g2..l.g2 + d].iter_mut().for_each(|x| *x = 1.0);
        }
        params[layout.gf..layout.gf + d]
            .iter_mut()
            .for_each(|x| *x = 1.0);
        Ok(Gpt {
            config,
            layout,
            params,
        })
    }

    pub fn n_params(&self) -> usize {
        self.params.len()
    }

    fn slice(&self, o: usize, n: usize) -> &[f64] {
        &self.params[o..o + n]
    }

    /// Run the model on `ids` (at most `ctx` tokens).
    pub fn forward(&self, ids: &[u32]) -> Forward {
        let c = &self.config;
        let (t, d, f, h, dh, v) = (ids.len(), c.dim, c.ff(), c.heads, c.head_dim(), c.vocab);
        assert!(t >= 1 && t <= c.ctx, "between 1 and ctx tokens");
        let mut x = vec![0.0; t * d];
        for (i, &id) in ids.iter().enumerate() {
            let te = self.slice(self.layout.tok_emb + (id as usize % v) * d, d);
            let pe = self.slice(self.layout.pos_emb + i * d, d);
            for j in 0..d {
                x[i * d + j] = te[j] + pe[j];
            }
        }
        let scale = 1.0 / (dh as f64).sqrt();
        let mut layers = Vec::with_capacity(c.layers);
        for lo in &self.layout.layers {
            let mut lc = LayerCache {
                x_in: x.clone(),
                r1: vec![0.0; t],
                n1: vec![0.0; t * d],
                q: vec![0.0; t * d],
                k: vec![0.0; t * d],
                v: vec![0.0; t * d],
                att: vec![0.0; h * t * t],
                ctx: vec![0.0; t * d],
                x1: vec![0.0; t * d],
                r2: vec![0.0; t],
                n2: vec![0.0; t * d],
                h1: vec![0.0; t * f],
                a: vec![0.0; t * f],
            };
            rmsnorm(&x, self.slice(lo.g1, d), t, d, &mut lc.n1, &mut lc.r1);
            matmul(&lc.n1, t, d, self.slice(lo.wq, d * d), d, &mut lc.q);
            matmul(&lc.n1, t, d, self.slice(lo.wk, d * d), d, &mut lc.k);
            matmul(&lc.n1, t, d, self.slice(lo.wv, d * d), d, &mut lc.v);
            for hd in 0..h {
                for i in 0..t {
                    let row = &mut lc.att[(hd * t + i) * t..(hd * t + i) * t + t];
                    let mut mx = f64::NEG_INFINITY;
                    for s in 0..=i {
                        let mut dot = 0.0;
                        for cc in 0..dh {
                            dot += lc.q[i * d + hd * dh + cc] * lc.k[s * d + hd * dh + cc];
                        }
                        row[s] = dot * scale;
                        mx = mx.max(row[s]);
                    }
                    let mut sum = 0.0;
                    for s in 0..=i {
                        row[s] = (row[s] - mx).exp();
                        sum += row[s];
                    }
                    for s in 0..=i {
                        row[s] /= sum;
                    }
                    for s in 0..=i {
                        let a = row[s];
                        for cc in 0..dh {
                            lc.ctx[i * d + hd * dh + cc] += a * lc.v[s * d + hd * dh + cc];
                        }
                    }
                }
            }
            let mut ao = vec![0.0; t * d];
            matmul(&lc.ctx, t, d, self.slice(lo.wo, d * d), d, &mut ao);
            for i in 0..t * d {
                lc.x1[i] = x[i] + ao[i];
            }
            rmsnorm(&lc.x1, self.slice(lo.g2, d), t, d, &mut lc.n2, &mut lc.r2);
            matmul(&lc.n2, t, d, self.slice(lo.w1, d * f), f, &mut lc.h1);
            let b1 = self.slice(lo.b1, f);
            for i in 0..t {
                for j in 0..f {
                    lc.h1[i * f + j] += b1[j];
                    lc.a[i * f + j] = gelu(lc.h1[i * f + j]);
                }
            }
            let mut m = vec![0.0; t * d];
            matmul(&lc.a, t, f, self.slice(lo.w2, f * d), d, &mut m);
            let b2 = self.slice(lo.b2, d);
            for i in 0..t {
                for j in 0..d {
                    x[i * d + j] = lc.x1[i * d + j] + m[i * d + j] + b2[j];
                }
            }
            layers.push(lc);
        }
        let mut nf = vec![0.0; t * d];
        let mut rf = vec![0.0; t];
        rmsnorm(&x, self.slice(self.layout.gf, d), t, d, &mut nf, &mut rf);
        // logits = nf * tok_emb^T (the embedding matrix is shared with the input)
        let mut logits = vec![0.0; t * v];
        let emb = self.slice(self.layout.tok_emb, v * d);
        for i in 0..t {
            for tok in 0..v {
                let mut s = 0.0;
                for j in 0..d {
                    s += nf[i * d + j] * emb[tok * d + j];
                }
                logits[i * v + tok] = s;
            }
        }
        Forward {
            logits,
            layers,
            xf: x,
            rf,
            nf,
        }
    }

    /// Mean cross-entropy of predicting `targets[i]` after seeing `inputs[..=i]`, and (when `grad`
    /// is given) the gradient of the SUM of those losses times `scale`, added into `grad`.
    /// Returns the SUM of the per-position losses.
    pub fn loss_and_grad(
        &self,
        inputs: &[u32],
        targets: &[u32],
        grad: Option<(&mut [f64], f64)>,
    ) -> f64 {
        let c = &self.config;
        let (t, d, f, h, dh, v) = (inputs.len(), c.dim, c.ff(), c.heads, c.head_dim(), c.vocab);
        let fw = self.forward(inputs);
        // softmax + cross-entropy
        let mut probs = fw.logits.clone();
        let mut loss = 0.0;
        for i in 0..t {
            let row = &mut probs[i * v..i * v + v];
            let mx = row.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let mut sum = 0.0;
            for p in row.iter_mut() {
                *p = (*p - mx).exp();
                sum += *p;
            }
            for p in row.iter_mut() {
                *p /= sum;
            }
            loss -= row[targets[i] as usize % v].max(1e-300).ln();
        }
        let Some((g, scale)) = grad else { return loss };
        let lay = &self.layout;
        // dL/dlogits
        let mut dlog = probs;
        for i in 0..t {
            dlog[i * v + targets[i] as usize % v] -= 1.0;
        }
        dlog.iter_mut().for_each(|x| *x *= scale);
        // logits = nf * emb^T
        let emb = self.slice(lay.tok_emb, v * d);
        let mut dnf = vec![0.0; t * d];
        for i in 0..t {
            for tok in 0..v {
                let gl = dlog[i * v + tok];
                if gl == 0.0 {
                    continue;
                }
                for j in 0..d {
                    dnf[i * d + j] += gl * emb[tok * d + j];
                    g[lay.tok_emb + tok * d + j] += gl * fw.nf[i * d + j];
                }
            }
        }
        let mut dx = vec![0.0; t * d];
        {
            let gf = lay.gf;
            let mut dg = vec![0.0; d];
            rmsnorm_back(
                &fw.xf,
                self.slice(gf, d),
                &fw.rf,
                &dnf,
                t,
                d,
                &mut dx,
                &mut dg,
            );
            for j in 0..d {
                g[gf + j] += dg[j];
            }
        }
        let scale_att = 1.0 / (dh as f64).sqrt();
        for (li, lo) in lay.layers.iter().enumerate().rev() {
            let lc = &fw.layers[li];
            // x_out = x1 + a*W2 + b2: dx is the gradient wrt x_out
            let dm = dx.clone(); // through the MLP branch
            let mut da = vec![0.0; t * f];
            acc_at_b(&lc.a, t, f, &dm, d, &mut g[lo.w2..lo.w2 + f * d]);
            for i in 0..t {
                for j in 0..d {
                    g[lo.b2 + j] += dm[i * d + j];
                }
            }
            acc_g_bt(&dm, t, d, self.slice(lo.w2, f * d), f, &mut da);
            let mut dh1 = vec![0.0; t * f];
            for i in 0..t * f {
                dh1[i] = da[i] * gelu_grad(lc.h1[i]);
            }
            acc_at_b(&lc.n2, t, d, &dh1, f, &mut g[lo.w1..lo.w1 + d * f]);
            for i in 0..t {
                for j in 0..f {
                    g[lo.b1 + j] += dh1[i * f + j];
                }
            }
            let mut dn2 = vec![0.0; t * d];
            acc_g_bt(&dh1, t, f, self.slice(lo.w1, d * f), d, &mut dn2);
            let mut dx1 = dx.clone(); // the residual path
            let mut dg2 = vec![0.0; d];
            rmsnorm_back(
                &lc.x1,
                self.slice(lo.g2, d),
                &lc.r2,
                &dn2,
                t,
                d,
                &mut dx1,
                &mut dg2,
            );
            for j in 0..d {
                g[lo.g2 + j] += dg2[j];
            }
            // x1 = x_in + ctx*Wo
            acc_at_b(&lc.ctx, t, d, &dx1, d, &mut g[lo.wo..lo.wo + d * d]);
            let mut dctx = vec![0.0; t * d];
            acc_g_bt(&dx1, t, d, self.slice(lo.wo, d * d), d, &mut dctx);
            let (mut dq, mut dk, mut dv) = (vec![0.0; t * d], vec![0.0; t * d], vec![0.0; t * d]);
            for hd in 0..h {
                for i in 0..t {
                    let arow = &lc.att[(hd * t + i) * t..(hd * t + i) * t + t];
                    // gradient wrt the attention weights, and wrt v
                    let mut datt = vec![0.0; i + 1];
                    for s in 0..=i {
                        let mut dot = 0.0;
                        for cc in 0..dh {
                            dot += dctx[i * d + hd * dh + cc] * lc.v[s * d + hd * dh + cc];
                            dv[s * d + hd * dh + cc] += arow[s] * dctx[i * d + hd * dh + cc];
                        }
                        datt[s] = dot;
                    }
                    let inner: f64 = (0..=i).map(|s| arow[s] * datt[s]).sum();
                    for s in 0..=i {
                        let dscore = arow[s] * (datt[s] - inner) * scale_att;
                        for cc in 0..dh {
                            dq[i * d + hd * dh + cc] += dscore * lc.k[s * d + hd * dh + cc];
                            dk[s * d + hd * dh + cc] += dscore * lc.q[i * d + hd * dh + cc];
                        }
                    }
                }
            }
            acc_at_b(&lc.n1, t, d, &dq, d, &mut g[lo.wq..lo.wq + d * d]);
            acc_at_b(&lc.n1, t, d, &dk, d, &mut g[lo.wk..lo.wk + d * d]);
            acc_at_b(&lc.n1, t, d, &dv, d, &mut g[lo.wv..lo.wv + d * d]);
            let mut dn1 = vec![0.0; t * d];
            acc_g_bt(&dq, t, d, self.slice(lo.wq, d * d), d, &mut dn1);
            acc_g_bt(&dk, t, d, self.slice(lo.wk, d * d), d, &mut dn1);
            acc_g_bt(&dv, t, d, self.slice(lo.wv, d * d), d, &mut dn1);
            let mut dx_in = dx1; // the residual path of the attention block
            let mut dg1 = vec![0.0; d];
            rmsnorm_back(
                &lc.x_in,
                self.slice(lo.g1, d),
                &lc.r1,
                &dn1,
                t,
                d,
                &mut dx_in,
                &mut dg1,
            );
            for j in 0..d {
                g[lo.g1 + j] += dg1[j];
            }
            dx = dx_in;
        }
        // x0 = tok_emb[id] + pos_emb[t]
        for (i, &id) in inputs.iter().enumerate() {
            let tok = id as usize % v;
            for j in 0..d {
                g[lay.tok_emb + tok * d + j] += dx[i * d + j];
                g[lay.pos_emb + i * d + j] += dx[i * d + j];
            }
        }
        loss
    }

    /// The embedding of every token: `vocab` rows of `dim` numbers.
    pub fn embeddings(&self) -> Vec<Vec<f64>> {
        let d = self.config.dim;
        (0..self.config.vocab)
            .map(|t| self.slice(self.layout.tok_emb + t * d, d).to_vec())
            .collect()
    }

    /// Probabilities of the next token after `ids` (the last `ctx` tokens are used).
    pub fn next_token_probs(&self, ids: &[u32]) -> Vec<f64> {
        let ids = if ids.len() > self.config.ctx {
            &ids[ids.len() - self.config.ctx..]
        } else {
            ids
        };
        let ids = if ids.is_empty() { &[0u32][..] } else { ids };
        let v = self.config.vocab;
        let fw = self.forward(ids);
        let last = &fw.logits[(ids.len() - 1) * v..ids.len() * v];
        let mx = last.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let mut p: Vec<f64> = last.iter().map(|x| (x - mx).exp()).collect();
        let s: f64 = p.iter().sum();
        p.iter_mut().for_each(|x| *x /= s);
        p
    }

    /// Memory the key/value cache of a real inference engine would need for `tokens` tokens of
    /// context with this shape, at `bytes` per number (2 for 16-bit).
    pub fn kv_cache_bytes(&self, tokens: usize, bytes: usize) -> usize {
        // 2 (keys and values) x layers x tokens x dim x bytes
        2 * self.config.layers * tokens * self.config.dim * bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny() -> Gpt {
        Gpt::new(
            Config {
                vocab: 7,
                dim: 8,
                heads: 2,
                layers: 2,
                ctx: 6,
                ff_mult: 2,
            },
            3,
        )
        .unwrap()
    }

    fn data() -> (Vec<u32>, Vec<u32>) {
        (vec![1, 4, 2, 6, 0], vec![4, 2, 6, 0, 3])
    }

    #[test]
    fn the_hand_written_gradients_match_numerical_differentiation() {
        let mut m = tiny();
        // make everything matter: gains away from 1, biases away from 0
        let mut rng = Rng::new(9);
        for p in m.params.iter_mut() {
            *p += rng.normal() * 0.15;
        }
        let (x, y) = data();
        let mut g = vec![0.0; m.params.len()];
        let loss = m.loss_and_grad(&x, &y, Some((&mut g, 1.0)));
        assert!(loss.is_finite() && loss > 0.0);
        // finite differences over parameters spread over every tensor
        let step = (m.params.len() / 150).max(1);
        let mut worst: f64 = 0.0;
        let mut checked = 0;
        let mut i = 0;
        while i < m.params.len() {
            let eps = 1e-5;
            let old = m.params[i];
            m.params[i] = old + eps;
            let lp = m.loss_and_grad(&x, &y, None);
            m.params[i] = old - eps;
            let lm = m.loss_and_grad(&x, &y, None);
            m.params[i] = old;
            let num = (lp - lm) / (2.0 * eps);
            let err = (num - g[i]).abs() / (num.abs() + g[i].abs() + 1e-7);
            worst = worst.max(err);
            assert!(
                err < 1e-5,
                "param {i}: analytic {} numeric {num} (relative error {err})",
                g[i]
            );
            checked += 1;
            i += step;
        }
        assert!(checked >= 100, "{checked} parameters were checked");
        eprintln!("gradient check: {checked} parameters, worst relative error {worst:.2e}");
    }

    #[test]
    fn a_fresh_model_guesses_about_evenly() {
        let m = tiny();
        let (x, y) = data();
        let mean = m.loss_and_grad(&x, &y, None) / x.len() as f64;
        let uniform = (m.config.vocab as f64).ln();
        assert!(
            (mean - uniform).abs() < 0.4,
            "{mean} vs ln(vocab) = {uniform}"
        );
    }

    #[test]
    fn attention_looks_only_backwards_and_every_row_sums_to_one() {
        let m = tiny();
        let fw = m.forward(&[1, 2, 3, 4]);
        for layer in 0..m.config.layers {
            let att = fw.attention(layer);
            let t = 4;
            for h in 0..m.config.heads {
                for i in 0..t {
                    let row = &att[(h * t + i) * t..(h * t + i) * t + t];
                    assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-12);
                    assert!(
                        row[i + 1..].iter().all(|&a| a == 0.0),
                        "no peeking at the future"
                    );
                }
            }
        }
    }

    #[test]
    fn what_comes_later_never_changes_what_is_predicted_earlier() {
        let m = tiny();
        let a = m.forward(&[1, 2, 3, 4, 5]).logits;
        let b = m.forward(&[1, 2, 3, 0, 0]).logits;
        let v = m.config.vocab;
        assert_eq!(
            a[..3 * v],
            b[..3 * v],
            "the first three positions only saw 1, 2, 3"
        );
        assert_ne!(a[3 * v..4 * v], b[3 * v..4 * v]);
    }

    #[test]
    fn sizes_are_validated_and_counted() {
        let bad = Config {
            vocab: 10,
            dim: 10,
            heads: 3,
            layers: 1,
            ctx: 8,
            ff_mult: 2,
        };
        assert!(bad
            .validate()
            .unwrap_err()
            .to_string()
            .contains("divisible"));
        assert!(Config {
            vocab: 10,
            dim: 9999,
            heads: 1,
            layers: 1,
            ctx: 8,
            ff_mult: 2
        }
        .validate()
        .is_err());
        let m = tiny();
        // 7*8 + 6*8 + gf 8 + 2 layers x (8+4*64+8+8*16+16+16*8+8)
        assert_eq!(
            m.n_params(),
            56 + 48 + 8 + 2 * (8 + 256 + 8 + 128 + 16 + 128 + 8)
        );
        assert_eq!(m.kv_cache_bytes(100, 2), 2 * 2 * 100 * 8 * 2);
    }
}
