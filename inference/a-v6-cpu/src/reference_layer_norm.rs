//! FP32, bias-free CPU LayerNorm matching the qualified ATen DEFAULT algorithm.
//! Port of RowwiseMoments (8 lanes, 16-vector chunks, Welford/cascade merges)
//! and LayerNormSecondPass at PyTorch 08187d9e0fba026dc8217405802ab5381dc88d90.
//! Provenance/license: ../../evidence/pytorch/. No parameter fitting or offsets.
use candle::{DType, Module, Result, Tensor};
use candle_nn::VarBuilder;

#[derive(Clone, Copy, Default)]
struct Moments {
    count: usize,
    mean: [f32; 8],
    m2: [f32; 8],
}

fn merge_vector(target: &mut Moments, add: Moments) {
    let n = target.count + add.count;
    let c = if n == 0 {
        0.
    } else {
        add.count as f32 / n as f32
    };
    for k in 0..8 {
        let delta = add.mean[k] - target.mean[k];
        #[cfg(feature = "mkl")]
        {
            // Training runtime's PyTorch 2.9.1, commit 5811a8d7..., AddMomentsVec.
            target.mean[k] += c * delta;
            target.m2[k] += add.m2[k] + delta * delta * c * target.count as f32;
        }
        #[cfg(not(feature = "mkl"))]
        {
        let m2_tmp = target.m2[k] + add.m2[k];
        let c_delta = c * delta;
        let m0_delta = delta * target.count as f32;
        target.mean[k] += c_delta;
        // ATen DEFAULT fmadd is a*b+c, not a fused instruction.
        target.m2[k] = m0_delta * c_delta + m2_tmp;
        }
    }
    target.count = n;
}

fn merge_scalar(
    count: &mut usize,
    mean: &mut f32,
    m2: &mut f32,
    n_add: usize,
    mean_add: f32,
    m2_add: f32,
) {
    let n = *count + n_add;
    let c = if n == 0 { 0. } else { n_add as f32 / n as f32 };
    let delta = mean_add - *mean;
    *mean += c * delta;
    *m2 += m2_add + delta * delta * c * *count as f32;
    *count = n;
}

pub fn row_moments(x: &[f32]) -> (f32, f32) {
    let vectors = x.len() / 8;
    let chunks = vectors.div_ceil(16);
    let depth = if chunks <= 1 {
        0
    } else {
        (usize::BITS - (chunks - 1).leading_zeros()) as usize
    };
    let mut stack = vec![Moments::default(); depth.max(1)];
    for i in 0..chunks {
        let count = 16.min(vectors - i * 16);
        let mut m = Moments {
            count,
            ..Moments::default()
        };
        for j in 0..count {
            let c = 1f32 / (j + 1) as f32;
            for k in 0..8 {
                let value = x[(i * 16 + j) * 8 + k];
                let delta = value - m.mean[k];
                m.mean[k] = c * delta + m.mean[k];
                m.m2[k] = delta * (value - m.mean[k]) + m.m2[k];
            }
        }
        merge_vector(&mut stack[0], m);
        let mut mask = i + 1;
        let mut j = 1;
        while j < depth && mask & 1 == 0 {
            let add = stack[j - 1];
            merge_vector(&mut stack[j], add);
            stack[j - 1] = Moments::default();
            mask >>= 1;
            j += 1;
        }
    }
    for i in 1..depth {
        let add = stack[i];
        merge_vector(&mut stack[0], add);
    }
    let (mut count, mut mean, mut m2) = (0, 0., 0.);
    for &value in &x[vectors * 8..] {
        let delta = value - mean;
        count += 1;
        mean += delta / count as f32;
        m2 += delta * (value - mean);
    }
    for k in 0..8 {
        merge_scalar(
            &mut count,
            &mut mean,
            &mut m2,
            vectors,
            stack[0].mean[k],
            stack[0].m2[k],
        );
    }
    (mean, m2 / x.len() as f32)
}

pub fn normalize(input: &[f32], weight: &[f32], eps: f32) -> Result<Vec<f32>> {
    if weight.is_empty()
        || !input.len().is_multiple_of(weight.len())
        || !eps.is_finite()
        || eps <= 0.
    {
        candle::bail!("invalid bias-free LayerNorm shape/epsilon");
    }
    let mut output = Vec::with_capacity(input.len());
    for row in input.chunks_exact(weight.len()) {
        let (mean, variance) = row_moments(row);
        let scale = 1f32 / (variance + eps).sqrt();
        let bias = -mean;
        output.extend(
            row.iter()
                .zip(weight)
                .map(|(&x, &gamma)| (x + bias) * scale * gamma + 0.),
        );
    }
    Ok(output)
}

#[derive(Clone, Debug)]
pub struct LayerNorm {
    weight: Vec<f32>,
    eps: f32,
    eps64: f64,
}
pub fn layer_norm_no_bias(size: usize, eps: f64, vb: VarBuilder) -> Result<LayerNorm> {
    Ok(LayerNorm {
        // FP32 checkpoints; an FP64 VarBuilder converts exactly and back.
        weight: vb.get(size, "weight")?.to_dtype(DType::F32)?.to_vec1::<f32>()?,
        eps: eps as f32,
        eps64: eps,
    })
}
/// FP64 bias-free LayerNorm (two-pass mean and population variance), used
/// only by the FP64 full-A numeric profile; not an ATen port.
pub fn normalize_f64(input: &[f64], weight: &[f32], eps: f64) -> Result<Vec<f64>> {
    if weight.is_empty()
        || !input.len().is_multiple_of(weight.len())
        || !eps.is_finite()
        || eps <= 0.
    {
        candle::bail!("invalid bias-free LayerNorm shape/epsilon");
    }
    let n = weight.len() as f64;
    let mut output = Vec::with_capacity(input.len());
    for row in input.chunks_exact(weight.len()) {
        let mean = row.iter().sum::<f64>() / n;
        let variance = row.iter().map(|x| (x - mean) * (x - mean)).sum::<f64>() / n;
        let scale = 1. / (variance + eps).sqrt();
        output.extend(
            row.iter()
                .zip(weight)
                .map(|(&x, &gamma)| (x - mean) * scale * f64::from(gamma)),
        );
    }
    Ok(output)
}

impl Module for LayerNorm {
    fn forward(&self, x: &Tensor) -> Result<Tensor> {
        if x.device().is_cpu()
            && x.dtype() == DType::F64
            && x.dims().last() == Some(&self.weight.len())
        {
            let input = x.flatten_all()?.to_vec1::<f64>()?;
            return Tensor::from_vec(
                normalize_f64(&input, &self.weight, self.eps64)?,
                x.shape(),
                x.device(),
            );
        }
        if !x.device().is_cpu()
            || x.dtype() != DType::F32
            || x.dims().last() != Some(&self.weight.len())
        {
            candle::bail!("reference LayerNorm requires CPU FP32 with matching final dimension");
        }
        let input = x.flatten_all()?.to_vec1::<f32>()?;
        Tensor::from_vec(
            normalize(&input, &self.weight, self.eps)?,
            x.shape(),
            x.device(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constant_and_non_multiple_vector_width_rows() {
        assert!(
            normalize(&[42.; 7], &[2.; 7], 1e-5)
                .unwrap()
                .iter()
                .all(|x| *x == 0.)
        );
        let values = (0..17).map(|i| i as f32 - 8.).collect::<Vec<_>>();
        let (mean, var) = row_moments(&values);
        assert!(mean.abs() <= f32::EPSILON);
        assert!((var - 24.).abs() < 1e-5);
        assert!(normalize(&values, &[1.; 8], 1e-5).is_err());
    }
    #[test]
    fn f64_profile_is_the_exact_two_pass_layer_norm() {
        let row = [1e4, -2., 3.5, 0.25, -1e4 + 1., 7., 0., 2.];
        let out = normalize_f64(&row, &[2.; 8], 1e-5).unwrap();
        let mean = row.iter().sum::<f64>() / 8.;
        let var = row.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / 8.;
        for (o, x) in out.iter().zip(row) {
            assert!((o - 2. * (x - mean) / (var + 1e-5).sqrt()).abs() < 1e-15);
        }
        assert!(normalize_f64(&row, &[1.; 3], 1e-5).is_err());
        assert!(normalize_f64(&row, &[1.; 8], 0.).is_err());
    }
}
