use anyhow::Result;
use candle_core::{DType, Device, Tensor};
use candle_nn::VarBuilder;
use candle_transformers::models::bert::{BertModel, Config};
use tokenizers::Tokenizer;

pub struct Embedder {
    model: BertModel,
    tokenizer: Tokenizer,
    device: Device,
}

impl Embedder {
    pub fn new() -> Result<Self> {
        let device = Device::Cpu;
        let model_dir = std::path::Path::new("models/bge-small");

        println!("Loading model from disk...");

        let config_str = std::fs::read_to_string(model_dir.join("config.json"))?;
        let config: Config = serde_json::from_str(&config_str)?;

        let tokenizer = Tokenizer::from_file(model_dir.join("tokenizer.json"))
            .map_err(|e| anyhow::anyhow!(e))?;

        let vb = unsafe {
            VarBuilder::from_mmaped_safetensors(
                &[model_dir.join("model.safetensors")],
                DType::F32,
                &device,
            )?
        };
        let model = BertModel::load(vb, &config)?;

        Ok(Self {
            model,
            tokenizer,
            device,
        })
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>> {
        let encoding = self
            .tokenizer
            .encode(text, true)
            .map_err(|e| anyhow::anyhow!(e))?;

        // Extract token data BEFORE converting to tensors
        let ids = encoding.get_ids().to_vec();
        let type_ids = encoding.get_type_ids().to_vec();
        let mask = encoding.get_attention_mask().to_vec();

        // Now convert to tensors
        let ids_tensor = Tensor::new(ids.as_slice(), &self.device)?.unsqueeze(0)?;
        let type_ids_tensor = Tensor::new(type_ids.as_slice(), &self.device)?.unsqueeze(0)?;
        let mask_tensor = Tensor::new(mask.as_slice(), &self.device)?.unsqueeze(0)?;

        // Run inference
        let output = self
            .model
            .forward(&ids_tensor, &type_ids_tensor, Some(&mask_tensor))?;

        // Mean pooling
        let (_batch, _seq, hidden) = output.dims3()?;
        let mask_f32 = mask_tensor.to_dtype(DType::F32)?;
        let mask_expanded = mask_f32.unsqueeze(2)?.broadcast_as((1, _seq, hidden))?;
        let sum = (output * &mask_expanded)?.sum(1)?;
        let count = mask_f32.sum(1)?.unsqueeze(1)?.broadcast_as((1, hidden))?;
        let mean = (sum / count)?;

        let embedding = mean.squeeze(0)?.to_vec1::<f32>()?;
        Ok(embedding)
    }
}
