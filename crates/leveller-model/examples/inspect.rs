//! Run the three graphs on a small span and print what comes back, so the
//! output order can be pinned down rather than guessed.
use tract_onnx::prelude::*;

fn run(path: std::path::PathBuf, inputs: Vec<Tensor>) -> TractResult<Vec<(String, Vec<usize>)>> {
    let mut model = tract_onnx::onnx().model_for_path(&path)?;
    for (i, t) in inputs.iter().enumerate() {
        model.set_input_fact(i, f32::fact(t.shape()).into())?;
    }
    let labels: Vec<String> = model
        .output_outlets()?
        .iter()
        .map(|o| model.node(o.node).name.clone())
        .collect();
    let runnable = model.into_optimized()?.into_runnable()?;
    let out = runnable.run(inputs.into_iter().map(TValue::from).collect())?;
    Ok(labels
        .into_iter()
        .zip(out.iter())
        .map(|(name, t)| (name, t.shape().to_vec()))
        .collect())
}

fn main() -> TractResult<()> {
    let dir = leveller_model::registry::model_path(&leveller_model::registry::MODELS[0]);
    let s = 8usize;

    let enc = run(
        dir.join("enc.onnx"),
        vec![
            Tensor::zero::<f32>(&[1, 1, s, 32])?,
            Tensor::zero::<f32>(&[1, 2, s, 96])?,
        ],
    )?;
    println!("== enc.onnx");
    for (i, (name, shape)) in enc.iter().enumerate() {
        println!("  out {i} {name:>34}  {shape:?}");
    }

    let erb = run(
        dir.join("erb_dec.onnx"),
        vec![
            Tensor::zero::<f32>(&[1, s, 512])?,
            Tensor::zero::<f32>(&[1, 64, s, 8])?,
            Tensor::zero::<f32>(&[1, 64, s, 8])?,
            Tensor::zero::<f32>(&[1, 64, s, 16])?,
            Tensor::zero::<f32>(&[1, 64, s, 32])?,
        ],
    )?;
    println!("== erb_dec.onnx");
    for (i, (name, shape)) in erb.iter().enumerate() {
        println!("  out {i} {name:>34}  {shape:?}");
    }

    let df = run(
        dir.join("df_dec.onnx"),
        vec![
            Tensor::zero::<f32>(&[1, s, 512])?,
            Tensor::zero::<f32>(&[1, 64, s, 96])?,
        ],
    )?;
    println!("== df_dec.onnx");
    for (i, (name, shape)) in df.iter().enumerate() {
        println!("  out {i} {name:>34}  {shape:?}");
    }
    Ok(())
}
