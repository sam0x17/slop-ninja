//! Exact six-tensor override for V6 layer21. The parent artifact and all earlier
//! blocks retain their immutable verifier; no model/operator source is changed.
use anyhow::{Result, ensure};
use candle::{DType, Device, Shape, Tensor};
use candle_nn::{VarBuilder, var_builder::SimpleBackend};
use std::{collections::HashMap, path::Path};

pub const SHAPES: [(&str, &[usize]); 6] = [
    ("attn_norm.weight", &[768]),
    ("attn.Wqkv.weight", &[2304, 768]),
    ("attn.Wo.weight", &[768, 768]),
    ("mlp_norm.weight", &[768]),
    ("mlp.Wi.weight", &[2304, 768]),
    ("mlp.Wo.weight", &[768, 1152]),
];
pub fn validate(tensors: &HashMap<String, Tensor>) -> Result<()> {
    ensure!(
        tensors.len() == SHAPES.len(),
        "only the six final-block tensors may be overlaid"
    );
    for (name, shape) in SHAPES {
        let t = tensors
            .get(name)
            .ok_or_else(|| anyhow::anyhow!("missing final-block tensor {name}"))?;
        ensure!(
            t.dtype() == DType::F32 && t.dims() == shape,
            "final-block tensor shape/dtype changed: {name}"
        );
        ensure!(
            t.flatten_all()?
                .to_vec1::<f32>()?
                .iter()
                .all(|x| x.is_finite()),
            "nonfinite final-block tensor"
        );
    }
    Ok(())
}
struct Overlay {
    parent: VarBuilder<'static>,
    tensors: HashMap<String, Tensor>,
}
impl Overlay {
    fn changed(&self, name: &str) -> candle::Result<Option<Tensor>> {
        match name.strip_prefix("model.layers.21.") {
            None => Ok(None),
            Some(key) => {
                self.tensors.get(key).cloned().map(Some).ok_or_else(|| {
                    candle::Error::Msg(format!("unqualified final-block tensor {key}"))
                })
            }
        }
    }
}
impl SimpleBackend for Overlay {
    fn get(
        &self,
        shape: Shape,
        name: &str,
        hint: candle_nn::Init,
        dtype: DType,
        device: &Device,
    ) -> candle::Result<Tensor> {
        let t = if let Some(t) = self.changed(name)? {
            t
        } else {
            return self.parent.get_with_hints_dtype(shape, name, hint, dtype);
        };
        if t.shape() != &shape {
            return Err(candle::Error::UnexpectedShape {
                msg: format!("overlay {name}"),
                expected: shape,
                got: t.shape().clone(),
            });
        }
        t.to_dtype(dtype)?.to_device(device)
    }
    fn get_unchecked(&self, name: &str, dtype: DType, device: &Device) -> candle::Result<Tensor> {
        match self.changed(name)? {
            Some(t) => t.to_dtype(dtype)?.to_device(device),
            None => self.parent.get_unchecked_dtype(name, dtype),
        }
    }
    fn contains_tensor(&self, name: &str) -> bool {
        if let Some(key) = name.strip_prefix("model.layers.21.") {
            self.tensors.contains_key(key)
        } else {
            self.parent.contains_tensor(name)
        }
    }
}
pub fn builder(parent: VarBuilder<'static>, path: &Path) -> Result<VarBuilder<'static>> {
    let tensors = candle::safetensors::load(path, &Device::Cpu)?;
    validate(&tensors)?;
    Ok(VarBuilder::from_backend(
        Box::new(Overlay { parent, tensors }),
        DType::F32,
        Device::Cpu,
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overlay_rejects_extra_earlier_layer_and_wrong_dtype() {
        let mut t = SHAPES
            .iter()
            .map(|(n, s)| Ok((n.to_string(), Tensor::zeros(*s, DType::F32, &Device::Cpu)?)))
            .collect::<candle::Result<HashMap<_, _>>>()
            .unwrap();
        validate(&t).unwrap();
        t.insert(
            "model.layers.0.attn_norm.weight".into(),
            Tensor::zeros(768, DType::F32, &Device::Cpu).unwrap(),
        );
        assert!(validate(&t).is_err());
        t.remove("model.layers.0.attn_norm.weight");
        t.insert(
            "attn_norm.weight".into(),
            Tensor::zeros(768, DType::F64, &Device::Cpu).unwrap(),
        );
        assert!(validate(&t).is_err());
    }
}
