use anyhow::{Context, Result};
use image::{RgbImage, imageops};
use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem};
use rten::Model;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static MODEL_TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq)]
pub struct OcrLine {
    pub text: String,
    pub confidence: f32,
    pub left: f32,
    pub top: f32,
    pub height: f32,
    pub width: f32,
}

pub struct Ocr {
    engine: Option<OcrEngine>,
    models_dir: Option<TemporaryModels>,
}

impl Ocr {
    pub fn new() -> Result<Self> {
        let models_dir = TemporaryModels(model_temp_dir()?);
        let root = &models_dir.0;
        let detection_path = root.join("text-detection.onnx");
        let recognition_path = root.join("text-recognition.onnx");
        std::fs::write(
            &detection_path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/models/text-detection.onnx"
            )),
        )
        .context("Cannot write embedded text-detection model")?;
        std::fs::write(
            &recognition_path,
            include_bytes!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/models/text-recognition.onnx"
            )),
        )
        .context("Cannot write embedded text-recognition model")?;

        let detection_model =
            Model::load_file(&detection_path).context("Cannot load text-detection model")?;
        let recognition_model =
            Model::load_file(&recognition_path).context("Cannot load text-recognition model")?;
        let engine = OcrEngine::new(OcrEngineParams {
            detection_model: Some(detection_model),
            recognition_model: Some(recognition_model),
            ..Default::default()
        })
        .context("Cannot initialize OCR engine")?;
        Ok(Self {
            engine: Some(engine),
            models_dir: Some(models_dir),
        })
    }

    pub fn read(&self, image: &RgbImage) -> Result<Vec<OcrLine>> {
        let source = ImageSource::from_bytes(image.as_raw(), image.dimensions())
            .context("Cannot prepare image for OCR")?;
        let input = self
            .engine
            .as_ref()
            .context("OCR engine is unavailable")?
            .prepare_input(source)
            .context("Cannot preprocess image for OCR")?;
        let engine = self.engine.as_ref().context("OCR engine is unavailable")?;
        let words = engine
            .detect_words(&input)
            .context("OCR text detection failed")?;
        let lines = engine.find_text_lines(&input, &words);
        let recognized = engine
            .recognize_text(&input, &lines)
            .context("OCR text recognition failed")?;

        let mut output = Vec::new();
        for line in recognized.into_iter().flatten() {
            let text = line.to_string().trim().to_owned();
            if text.is_empty() {
                continue;
            }
            let bounds = line.bounding_rect();
            output.push(OcrLine {
                text,
                confidence: 0.8,
                left: bounds.left() as f32,
                top: bounds.top() as f32,
                height: bounds.height() as f32,
                width: bounds.width() as f32,
            });
        }
        Ok(output)
    }

    pub fn read_bounded(&self, image: &RgbImage, max_dimension: u32) -> Result<Vec<OcrLine>> {
        let (width, height) = image.dimensions();
        let largest = width.max(height);
        if largest <= max_dimension {
            return self.read(image);
        }
        let scale = max_dimension as f32 / largest as f32;
        let resized = imageops::resize(
            image,
            (width as f32 * scale) as u32,
            (height as f32 * scale) as u32,
            imageops::FilterType::Triangle,
        );
        let mut lines = self.read(&resized)?;
        for line in &mut lines {
            line.left /= scale;
            line.top /= scale;
            line.height /= scale;
            line.width /= scale;
        }
        Ok(lines)
    }
}

impl Drop for Ocr {
    fn drop(&mut self) {
        drop(self.engine.take());
        drop(self.models_dir.take());
    }
}

struct TemporaryModels(PathBuf);

impl Drop for TemporaryModels {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            log::warn!(
                "Cannot remove temporary OCR model directory {}: {error}",
                self.0.display()
            );
        }
    }
}

fn model_temp_dir() -> Result<PathBuf> {
    let root = std::env::temp_dir().join("musicscansort");
    std::fs::create_dir_all(&root).context("Cannot create temporary OCR model root")?;
    loop {
        let id = MODEL_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let path = root.join(format!("{}-{id}", std::process::id()));
        match std::fs::create_dir(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).context("Cannot create temporary OCR model directory");
            }
        }
    }
}
