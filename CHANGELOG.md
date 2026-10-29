# Changelog

Bu projedeki önemli değişiklikler burada tutulur. Biçim [Keep a Changelog](https://keepachangelog.com/)
yaklaşımını, sürümler [SemVer](https://semver.org/) kurallarını izler.

## [Unreleased]

### Eklendi
- GitHub Pages tanıtım sayfası (`index.html`), TR/EN dil desteğiyle.
- `.gitattributes` (satır sonları, ikili dosyalar).

### Değişti
- README, Türkçe öncelikli ve iki dilli rehber düzenine geçirildi.
- Windows dışı derlemelerde çark olayları için ölü kod uyarısı susturuldu (clippy `-D warnings` artık her platformda temiz).

## [0.1.0] - 2026-10-25

### Eklendi
- Sistem tepsisi uygulaması: dinamik durum satırları, canlı ilerleme, oturum istatistikleri.
- Görsel sıkıştırma ve dönüştürme: PNG, JPEG, WebP, AVIF (yalnızca çıktı), TIFF, BMP, ICO.
- EXIF/XMP temizleme, maksimum çözünürlük ve kalite ayarları.
- PDF optimizasyonu ve görsellerden PDF oluşturma.
- İzlenen klasör ve pano sıkıştırma.
- Shift + sürükle format çarkı (Windows), glass temalı arayüz, gömülü Outfit yazı tipi.
- CI (fmt, clippy, test) ve etiketle tetiklenen Windows sürüm iş akışı.

[Unreleased]: https://github.com/BeyazAdam/mandalina-rs/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/BeyazAdam/mandalina-rs/releases/tag/v0.1.0
