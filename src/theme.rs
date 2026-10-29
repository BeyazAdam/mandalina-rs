//! Görsel kimlik: renk paleti ve gömülü yazı tipi.
//!
//! Palet, BeyazAdam/unreal-engine-version-guide sayfasının "glass" temasından
//! alınmıştır: koyu lacivert cam zemin, %8 beyaz kenarlıklar, mavi → indigo
//! vurgu gradyanı (Tailwind `blue-600` → `indigo-600`) ve slate metin tonları.

use std::sync::OnceLock;

use ab_glyph::{FontVec, VariableFont};

/// Ön-çarpılmamış RGBA renk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

impl Rgba {
    pub const fn with_alpha(self, alpha: u8) -> Self {
        Self(self.0, self.1, self.2, alpha)
    }
}

/// Referans sayfanın `body` gradyanı ve `.glass` paneli.
// Yalnızca README önizlemesindeki sayfa zemininde kullanılır.
#[cfg_attr(not(test), allow(dead_code))]
pub const BG_TOP: Rgba = Rgba(0x1e, 0x1e, 0x38, 255);
pub const BG_BOTTOM: Rgba = Rgba(0x0d, 0x0d, 0x15, 255);
pub const GLASS: Rgba = Rgba(20, 20, 35, 255);
pub const WHITE: Rgba = Rgba(255, 255, 255, 255);

/// Vurgu: `from-blue-600 to-indigo-600`, parlama `blue-500`.
pub const BLUE_600: Rgba = Rgba(0x25, 0x63, 0xeb, 255);
pub const INDIGO_600: Rgba = Rgba(0x4f, 0x46, 0xe5, 255);
pub const BLUE_500: Rgba = Rgba(0x3b, 0x82, 0xf6, 255);
pub const INDIGO_500: Rgba = Rgba(0x63, 0x66, 0xf1, 255);
pub const BLUE_400: Rgba = Rgba(0x60, 0xa5, 0xfa, 255);

/// Metin tonları.
pub const SLATE_100: Rgba = Rgba(0xf1, 0xf5, 0xf9, 255);
pub const SLATE_300: Rgba = Rgba(0xcb, 0xd5, 0xe1, 255);
pub const SLATE_500: Rgba = Rgba(0x64, 0x74, 0x8b, 255);
pub const SLATE_600: Rgba = Rgba(0x47, 0x55, 0x69, 255);

/// Durum renkleri.
pub const GREEN_500: Rgba = Rgba(0x22, 0xc5, 0x5e, 255);
pub const GREEN_400: Rgba = Rgba(0x4a, 0xde, 0x80, 255);
pub const RED_500: Rgba = Rgba(0xef, 0x44, 0x44, 255);
pub const RED_400: Rgba = Rgba(0xf8, 0x71, 0x71, 255);

/// Outfit (SIL OFL 1.1), değişken ağırlıklı. Lisans: `assets/fonts/OFL.txt`.
const OUTFIT: &[u8] = include_bytes!("../assets/fonts/Outfit-Variable.ttf");

#[derive(Debug, Clone, Copy)]
pub enum Weight {
    SemiBold,
    Bold,
}

/// Belirtilen ağırlıkta Outfit. Yazı tipi ikiliye gömülü olduğu için
/// çalışma anında sistemde bulunması gerekmez.
pub fn font(weight: Weight) -> &'static FontVec {
    static SEMIBOLD: OnceLock<FontVec> = OnceLock::new();
    static BOLD: OnceLock<FontVec> = OnceLock::new();
    let (cell, wght) = match weight {
        Weight::SemiBold => (&SEMIBOLD, 600.0),
        Weight::Bold => (&BOLD, 700.0),
    };
    cell.get_or_init(|| {
        let mut font = FontVec::try_from_vec(OUTFIT.to_vec()).expect("gömülü Outfit yazı tipi geçerli olmalı");
        font.set_variation(b"wght", wght);
        font
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ab_glyph::Font;

    #[test]
    fn outfit_covers_turkish_ui_text() {
        let font = font(Weight::Bold);
        for ch in "Sıkıştır İşleniyor… Hata ğüöçşıİĞÜÖÇŞ 0123456789,KMB"
            .chars()
            .filter(|c| !c.is_whitespace())
        {
            assert_ne!(font.glyph_id(ch).0, 0, "Outfit'te '{ch}' karakteri yok");
        }
    }
}
