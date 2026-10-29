//! Çalışma zamanı ayarları ve çıktı yolu kuralları.

use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use crate::image_compressor::{ImageFormatKind, ImageOptions};
use crate::pdf_compressor::PdfOptions;

/// Sıkıştırılmış dosyaların yazıldığı alt klasör. İzleyici özyinelemeli
/// çalışmadığı için buraya yazılan dosyalar yeniden işlenmez.
pub const OUTPUT_DIR_NAME: &str = "Sıkıştırılmış";

const WATCH_DIR_NAME: &str = "Sıkıştırılacaklar";

#[derive(Debug, Clone)]
pub struct Settings {
    /// Kayıplı kodlayıcılar için kalite (1-100).
    pub quality: u8,
    /// Uzun kenar sınırı (piksel). `None` ise çözünürlük korunur.
    pub max_dimension: Option<u32>,
    /// PDF içindeki görseller için uzun kenar sınırı.
    pub pdf_max_dimension: Option<u32>,
    /// Görselleri bu formata dönüştür. `None` ise kaynak format korunur.
    pub convert_to: Option<ImageFormatKind>,
    /// Otomatik izlenen klasör.
    pub watch_dir: PathBuf,
    /// Uygulama açılırken klasör izleme başlasın mı.
    pub watch_on_start: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let base = dirs::picture_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            quality: 80,
            max_dimension: None,
            pdf_max_dimension: Some(2000),
            convert_to: None,
            watch_dir: base.join(WATCH_DIR_NAME),
            watch_on_start: true,
        }
    }
}

impl Settings {
    pub fn image_options(&self) -> ImageOptions {
        ImageOptions {
            quality: self.quality,
            max_width: self.max_dimension,
            max_height: self.max_dimension,
            convert_to: self.convert_to,
        }
    }

    pub fn pdf_options(&self) -> PdfOptions {
        PdfOptions {
            image: ImageOptions {
                quality: self.quality,
                max_width: self.pdf_max_dimension,
                max_height: self.pdf_max_dimension,
                // PDF içinde gömülü görseller her zaman JPEG (DCTDecode) olarak yazılır.
                convert_to: None,
            },
            ..PdfOptions::default()
        }
    }

    /// `foto/kedi.png` -> `foto/Sıkıştırılmış/kedi.<hedef uzantı>`
    pub fn output_path(&self, input: &Path) -> PathBuf {
        let dir = input
            .parent()
            .map_or_else(|| PathBuf::from(OUTPUT_DIR_NAME), |p| p.join(OUTPUT_DIR_NAME));
        let stem = input.file_stem().unwrap_or_default().to_string_lossy();
        let ext = match (ImageFormatKind::decodable_from_path(input), self.convert_to) {
            (Some(_), Some(target)) => target.extension().to_owned(),
            _ => input
                .extension()
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        // `with_extension` kullanılmıyor: "a.b.png" gibi adlarda ".b" kısmını silerdi.
        if ext.is_empty() {
            dir.join(stem.as_ref())
        } else {
            dir.join(format!("{stem}.{ext}"))
        }
    }

    /// Pano görsellerinin diske kaydedildiği klasör.
    pub fn clipboard_output_dir(&self) -> PathBuf {
        self.watch_dir.join(OUTPUT_DIR_NAME)
    }
}

/// Paylaşılan ayarların anlık kopyası. Kilit zehirlenmişse (bir iş parçacığı
/// panikle kapanmışsa) son değer yine de kullanılır.
pub fn snapshot(settings: &RwLock<Settings>) -> Settings {
    settings.read().unwrap_or_else(PoisonError::into_inner).clone()
}
