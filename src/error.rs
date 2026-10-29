//! Sıkıştırma modüllerinin ortak hata türü.
//!
//! Kütüphane benzeri modüller (`image_compressor`, `pdf_compressor`) tipli
//! `CompressError` döndürür; uygulama katmanı (`main`, `jobs`, `watcher`)
//! bağlam eklemek için `anyhow` kullanır.

use std::path::PathBuf;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum CompressError {
    #[error("desteklenmeyen dosya türü: {}", .0.display())]
    Unsupported(PathBuf),

    #[error("görsel işlenemedi: {0}")]
    Image(#[from] image::ImageError),

    #[error("PDF işlenemedi: {0}")]
    Pdf(#[from] lopdf::Error),

    #[error("dosya hatası: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, CompressError>;
