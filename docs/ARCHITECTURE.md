# Mimari / Architecture

> Bu belge geliştiriciler içindir (Türkçe). Kullanım için [README](../README.md) dosyasına bakın.

## Proje yapısı

```
mandalina-rs/
├── .github/workflows/
│   ├── ci.yml              # fmt + clippy (-D warnings) + test, Windows
│   └── release.yml         # v* etiketi → Windows exe'si GitHub Release'e
├── assets/
│   ├── fonts/              # Outfit (SIL OFL 1.1) — ikiliye gömülür
│   ├── screenshots/        # README görselleri (test ile üretilir)
│   ├── icon.png            # logo (test ile üretilir)
│   └── icon.ico            # exe simgesi, 16–256 px (test ile üretilir)
├── build.rs                # Windows: exe'ye simge + yapımcı bilgisi (BeyazAdam) gömer
├── docs/ARCHITECTURE.md
├── src/
│   ├── main.rs             # tao olay döngüsü, App durumu
│   ├── events.rs           # AppEvent + EventSink (iş parçacıkları → ana döngü)
│   ├── tray.rs             # tepsi simgesi, dinamik menü, radyo grupları
│   ├── jobs.rs             # rayon ile toplu işleme, raporlar, bildirimler
│   ├── image_compressor.rs # PNG/JPEG/WebP/AVIF/TIFF/BMP/ICO
│   ├── pdf_compressor.rs   # PDF görsel optimizasyonu, görsel → PDF
│   ├── watcher.rs          # izlenen klasör + pano
│   ├── theme.rs            # renk paleti + gömülü yazı tipi
│   ├── config.rs · error.rs · util.rs
│   └── wheel/
│       ├── mod.rs              # geometri, isabet testi, durum makinesi
│       ├── render.rs           # tiny-skia çizimi
│       ├── platform_windows.rs # katmanlı pencere, IDropTarget, Shift tetikleyici
│       └── platform_stub.rs    # diğer platformlar
├── index.html              # GitHub Pages tanıtım sayfası (TR/EN)
├── CHANGELOG.md · LICENSE · README.md
└── Cargo.toml
```

## Yapımcı imzası

Yapımcı adı tek yerden gelir: `Cargo.toml` → `authors = ["BeyazAdam"]`. `main.rs` bunu
`AUTHOR` / `SIGNATURE` sabitlerine çevirir ve şu yerlerde gösterilir:

- Çarkın göbeğinde, hapın altında: `by BeyazAdam`
- Tepsi menüsünün en üst satırı: `Mandalina v0.1.0 · by BeyazAdam`, en altta `GitHub'da aç · BeyazAdam`
- Tepsi simgesinin ipucu (tooltip) ve başlangıç logu
- `mandalina.exe` → Özellikler → Ayrıntılar: Şirket ve Telif hakkı (`build.rs`)

## İş parçacığı modeli

```
            ┌──────────────── ana iş parçacığı (tao olay döngüsü) ────────────────┐
 tıklama ──►│ MenuEvent ─► AppEvent::Menu ─► App::handle ─► TrayMenu / Wheel      │
 sürükle ──►│ IDropTarget ─► AppEvent::Wheel ─┘                                   │
            └─────────────▲───────────────────────────────────────────────────────┘
                          │ EventSink (EventLoopProxy, Send + Sync)
   ┌──────────────────────┼──────────────────────┬──────────────────────┐
   │ jobs (rayon havuzu)  │ watcher: notify      │ watcher: pano        │ wheel: Shift
   │ JobStarted/Progress/ │ ─► spawn_file_job    │ ─► run_clipboard_job │ tetikleyici
   │ JobFinished          │                      │                      │
   └──────────────────────┴──────────────────────┴──────────────────────┘
```

Tepsi menü öğeleri `!Send` olduğu için (Windows'ta `Rc` tabanlı) **yalnızca ana iş parçacığı**
menüye ve çark penceresine dokunur. Arka plan işleri `AppEvent` gönderir; ana döngü
`ControlFlow::Wait` ile uyur ve yalnızca olay geldiğinde uyanır. Çark açıkken kapanma zamanı için
`ControlFlow::WaitUntil` kullanılır.

## Shift + sürükle çarkı

- `wheel/mod.rs`: geometri, isabet testi, durum makinesi (Açık → İşleniyor → Bitti → Kapalı).
  Platformdan bağımsızdır ve birim testleri vardır.
- `wheel/render.rs`: `tiny-skia` ile çizim, gömülü Outfit yazı tipiyle (`ab_glyph`) metin.
  Yarı saydam şekiller önce opak bir katmana çizilip tek opaklıkla birleştirilir; böylece
  dolgu ile yuvarlatma konturu örtüştüğünde alfa ikiye katlanmaz. Kenarlıklar, tam şekil çizilip
  içi `BlendMode::Clear` ile silinerek halka olarak elde edilir.
- `wheel/platform_windows.rs`: `UpdateLayeredWindow` ile piksel başına alfa kullanan, odak
  çalmayan (`WS_EX_NOACTIVATE`) pencere; OLE `IDropTarget`; `GetAsyncKeyState` ile Shift ve fare
  tuşunu yoklayan tetikleyici. Genel klavye kancası (hook) kurulmaz.
- **Güvenlik:** Shift basılıyken Explorer sürüklemeyi "taşı" olarak başlatır. Bırakma hedefi her
  zaman yalnızca `DROPEFFECT_COPY` döndürür; böylece kaynak dosya asla silinmez. (tao'nun hazır
  bırakma desteği `Drop` içinde efekti ayarlamadığı için kullanılmadı.)

Tasarım önizlemeleri ve README görselleri:

```bash
cargo test render_preview -- --ignored
```

```bash
cargo test icon_preview -- --ignored
```

Gerçek katmanlı pencereyi ekranda 4 saniye göstermek için:

```bash
cargo test overlay_on_screen -- --ignored
```

## Renk paleti

`theme.rs`, [unreal-engine-version-guide](https://github.com/BeyazAdam/unreal-engine-version-guide)
sayfasının "glass" temasını izler:

| Rol | Değer |
|---|---|
| Zemin gradyanı | `#1e1e38` → `#0d0d15` |
| Cam panel | `rgba(20, 20, 35, 0.65)`, kenarlık `rgba(255, 255, 255, 0.08)` |
| Vurgu | `blue-600 #2563eb` → `indigo-600 #4f46e5`, parlama `blue-500 #3b82f6` |
| Metin | `slate-100 #f1f5f9`, `slate-300 #cbd5e1`, devre dışı `slate-600 #475569` |
| Durum | başarı `green-500/400`, hata `red-500/400` |
| Yazı tipi | Outfit 600/700 |

## Dinamik menü güncelleme

### İşçi iş parçacığından ilerleme bildirme

İşçi menüye doğrudan dokunmaz, olay gönderir (`jobs.rs`):

```rust
let n = done.fetch_add(1, Ordering::Relaxed) + 1;
sink.send(AppEvent::JobProgress { id, done: n, total });
```

### Ana döngüde olayı işleyip menüyü güncelleme

```rust
AppEvent::JobProgress { id, done, total } => {
    if let Some(job) = self.active.get_mut(&id) {
        job.done = done;
        job.total = total;
    }
    self.refresh_tray();
}

fn refresh_tray(&self) {
    let Some(tray) = &self.tray else { return };
    match self.active.iter().min_by_key(|(id, _)| **id) {
        Some((_, job)) => tray.show_progress(&job.label, job.done, job.total, self.active.len() - 1),
        None => tray.show_idle(&self.stats),
    }
}
```

### Menü metni, simge ve tooltip

`status` ve `stats` devre dışı (tıklanamayan) bilgi satırlarıdır:

```rust
// İş sürerken: "⏳ Fotoğraflar: işleniyor… 3/10 (+1 iş)"
self.status.set_text(&text);
self.set_tooltip(&format!("Mandalina – {text}"));
self.set_icon(IconState::Busy); // sağ altta yeşil durum noktası

// Bitince: "✓ Fotoğraflar: 10 dosya · 18,4 MB tasarruf (%63)"
self.stats.set_text(&format!("Toplam: {} dosya · {} tasarruf", stats.files, ...));
self.open_last.set_enabled(true);
```

### Radyo düğmesi gibi davranan alt menü

`muda` onay kutusunu tıklamada kendiliğinden değiştirir; `RadioGroup::select` grubun geri kalanını
temizler:

```rust
} else if let Some(q) = self.quality.find(id) {
    self.quality.select(q);
    MenuAction::SetQuality(q)
}
```

## Bilinen sınırlamalar

- **Çark yalnızca Windows'ta.** Çark boyutu sistem DPI'sına göre ayarlanır; farklı DPI'lı ikinci
  monitörde biraz büyük ya da küçük görünebilir.
- **HEIC ve DOCX yok:** HEIC için açık, saf Rust bir kodlayıcı yok (patentli); DOCX bir belge
  motoru gerektirir.
- **AVIF yalnızca çıktı:** okumak için C kütüphanesi (dav1d) gerekir.
- **WebP kayıpsız:** kayıplı WebP için `webp` (libwebp) crate'i eklenebilir.
- **PNG:** kalite ayarı yalnızca 16 → 8 bit indirgemeyi açar; gerçek kayıplı PNG için
  `imagequant` + `oxipng` eklenebilir.
- **PDF:** CMYK, Indexed, maskeli ve JBIG2/JPX görseller atlanır.
- **Pano:** pano ham RGBA tutar; asıl tasarruf `Sıkıştırılacaklar/Sıkıştırılmış/pano-*` dosyasında
  görülür. Otomatik izleme panoyu 1,2 sn'de bir yoklar.
- **Ayarlar kalıcı değil:** `serde` + `toml` ile `dirs::config_dir()` altına kaydetmek sonraki adım.
