//! Çarkı `tiny_skia::Pixmap` üzerine çizer (ön-çarpılmış RGBA).
//!
//! Görünüm `theme` modülündeki "glass" paletini izler: koyu cam disk, içinde
//! mavi/indigo ortam ışıkları, ince beyaz kenarlıklı dilimler ve vurgulanan
//! dilimde mavi → indigo gradyan ile hafif parlama.
//!
//! Yuvarlak köşeli dilim tekniği: dilim, köşe yarıçapı kadar içeri çekilmiş
//! bir halka dilimi olarak doldurulur ve aynı renkle, iki katı kalınlıkta,
//! yuvarlak birleşimli (round join) bir kontur çizilir. Kontur kalınlığını
//! değiştirmek şekli her yönden eşit büyütür/küçültür; kenarlık ve parlama
//! bu sayede elde edilir.

#[cfg(test)]
use std::path::PathBuf;

use ab_glyph::{point, Font, FontVec, Glyph, PxScale, ScaleFont};
use tiny_skia::{
    BlendMode, Color, FillRule, GradientStop, LineCap, LineJoin, LinearGradient, Mask, Paint, Path, PathBuilder,
    Pixmap, PixmapPaint, Point, RadialGradient, Shader, SpreadMode, Stroke, Transform,
};

use super::{Geometry, Hit, Phase, WheelModel, SEGMENTS};
use crate::theme::{self, Rgba, Weight};

/// Bir şeklin dolgu ve kenarlık renkleri.
#[derive(Debug, Clone, Copy)]
enum Fill {
    Solid(Rgba),
    /// Mavi → indigo vurgu gradyanı (opaklık ayrıca verilir).
    Accent(u8),
}

#[derive(Debug, Clone, Copy)]
struct Style {
    fill: Fill,
    border: Rgba,
    /// Dış parlama (yalnızca vurgulu öğelerde).
    glow: Option<Rgba>,
    text: Rgba,
}

const BORDER_WIDTH: f32 = 1.4;

const PETAL: Style = Style {
    fill: Fill::Solid(theme::WHITE.with_alpha(16)),
    border: theme::WHITE.with_alpha(30),
    glow: None,
    text: theme::SLATE_100,
};
const PETAL_DISABLED: Style = Style {
    fill: Fill::Solid(theme::WHITE.with_alpha(3)),
    border: theme::WHITE.with_alpha(10),
    glow: None,
    text: theme::SLATE_600,
};
const ACCENT: Style = Style {
    fill: Fill::Accent(240),
    border: theme::BLUE_500.with_alpha(210),
    glow: Some(theme::BLUE_500),
    text: theme::WHITE,
};
const PILL: Style = Style {
    fill: Fill::Solid(theme::WHITE.with_alpha(18)),
    border: theme::WHITE.with_alpha(30),
    glow: None,
    text: theme::SLATE_300,
};
const PILL_BUSY: Style = Style {
    fill: Fill::Solid(theme::BLUE_500.with_alpha(45)),
    border: theme::BLUE_500.with_alpha(130),
    glow: None,
    text: theme::BLUE_400,
};
const PILL_OK: Style = Style {
    fill: Fill::Solid(theme::GREEN_500.with_alpha(45)),
    border: theme::GREEN_500.with_alpha(130),
    glow: Some(theme::GREEN_500),
    text: theme::GREEN_400,
};
const PILL_ERR: Style = Style {
    fill: Fill::Solid(theme::RED_500.with_alpha(45)),
    border: theme::RED_500.with_alpha(130),
    glow: Some(theme::RED_500),
    text: theme::RED_400,
};

pub struct Renderer {
    label_font: &'static FontVec,
    pill_font: &'static FontVec,
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new()
    }
}

impl Renderer {
    pub fn new() -> Self {
        Self {
            label_font: theme::font(Weight::Bold),
            pill_font: theme::font(Weight::SemiBold),
        }
    }

    pub fn render(&self, geo: Geometry, model: &WheelModel) -> Pixmap {
        let size = geo.size_px();
        let mut pixmap = Pixmap::new(size, size).expect("çark boyutu sıfır olamaz");
        // Yarı saydam şekilleri tek opaklıkla birleştirmek için yeniden kullanılan katman.
        let mut layer = Pixmap::new(size, size).expect("çark boyutu sıfır olamaz");
        let s = geo.scale;
        let c = geo.center();
        let disc_r = Geometry::DISC * s;

        draw_disc(&mut pixmap, c, disc_r, s);

        for (i, target) in SEGMENTS.iter().enumerate() {
            let hit = Hit::Segment(i);
            let style = match (model.is_enabled(hit), model.hover == Some(hit)) {
                (true, true) => ACCENT,
                (true, false) => PETAL,
                (false, _) => PETAL_DISABLED,
            };
            let petal = petal_path(geo, i);
            draw_rounded(
                &mut pixmap,
                &mut layer,
                &petal,
                Geometry::ROUNDING * s,
                style,
                (c, disc_r),
                s,
            );

            let angle = Geometry::segment_angle(i);
            let r = (Geometry::RING_IN + Geometry::RING_OUT) / 2.0 * s;
            draw_text(
                &mut pixmap,
                self.label_font,
                target.label(),
                24.0 * s,
                (c + r * angle.cos(), c + r * angle.sin()),
                style.text,
            );
        }

        // Göbek: diskten biraz daha koyu cam, ince kenarlık.
        let hub = circle(c, c, Geometry::HUB * s);
        fill_path(&mut pixmap, &hub, theme::BG_BOTTOM.with_alpha(150));
        stroke_path(&mut pixmap, &hub, theme::WHITE.with_alpha(18), BORDER_WIDTH * s);

        let pill_style = match &model.phase {
            Phase::Done { ok: true, .. } => PILL_OK,
            Phase::Done { ok: false, .. } => PILL_ERR,
            Phase::Busy { .. } => PILL_BUSY,
            _ if model.hover == Some(Hit::Center) => ACCENT,
            _ => PILL,
        };
        // Hap: yuvarlak uçlu kalın bir çizgi; yarıçap = yarı yükseklik.
        let radius = 26.0 * s;
        let half_len = 66.0 * s - radius;
        let mut pb = PathBuilder::new();
        pb.move_to(c - half_len, c);
        pb.line_to(c + half_len, c);
        let pill = pb.finish().expect("hap yolu boş olamaz");
        draw_rounded(&mut pixmap, &mut layer, &pill, radius, pill_style, (c, disc_r), s);
        draw_text(
            &mut pixmap,
            self.pill_font,
            &model.hub_text(),
            21.0 * s,
            (c, c),
            pill_style.text,
        );

        // Yapımcı imzası: göbekte, hapın altında, göze batmayan küçük bir satır.
        draw_text(
            &mut pixmap,
            self.pill_font,
            crate::SIGNATURE,
            12.0 * s,
            (c, c + 50.0 * s),
            theme::SLATE_500,
        );

        pixmap
    }
}

/// Gölge + koyu cam disk + diskin içine kırpılmış mavi/indigo ortam ışıkları.
fn draw_disc(pixmap: &mut Pixmap, c: f32, r: f32, s: f32) {
    for i in 0..14 {
        let spread = (14 - i) as f32 * s;
        fill_path(pixmap, &circle(c, c + 6.0 * s, r + spread), Rgba(0, 0, 0, 7));
    }
    let disc = circle(c, c, r);
    fill_path(pixmap, &disc, theme::GLASS.with_alpha(222));

    let mut mask = Mask::new(pixmap.width(), pixmap.height()).expect("maske boyutu sıfır olamaz");
    mask.fill_path(&disc, FillRule::Winding, true, Transform::identity());
    // Referans sayfadaki `bg-blue-500/10` ve `bg-indigo-500/10` bulanık lekeler.
    for (color, (dx, dy)) in [(theme::BLUE_500, (-0.38, -0.42)), (theme::INDIGO_500, (0.40, 0.40))] {
        let center = Point::from_xy(c + dx * r, c + dy * r);
        let shader = RadialGradient::new(
            center,
            0.0,
            center,
            r * 0.95,
            vec![
                GradientStop::new(0.0, color_of(color.with_alpha(52))),
                GradientStop::new(1.0, color_of(color.with_alpha(0))),
            ],
            SpreadMode::Pad,
            Transform::identity(),
        );
        if let Some(shader) = shader {
            let paint = Paint {
                shader,
                anti_alias: true,
                ..Paint::default()
            };
            pixmap.fill_path(&disc, &paint, FillRule::Winding, Transform::identity(), Some(&mask));
        }
    }
    stroke_path(
        pixmap,
        &circle(c, c, r - 0.75 * s),
        theme::WHITE.with_alpha(22),
        BORDER_WIDTH * s,
    );
}

/// `path` + `radius` kalınlıkta yuvarlak kontur ile tanımlanan şekli
/// parlama, kenarlık ve dolguyla çizer. Her katman önce opak çizilip tek
/// opaklıkla birleştirilir; böylece örtüşen dolgu/kontur alfayı ikiye katlamaz.
fn draw_rounded(
    pixmap: &mut Pixmap,
    layer: &mut Pixmap,
    path: &Path,
    radius: f32,
    style: Style,
    (c, disc_r): (f32, f32),
    s: f32,
) {
    if let Some(glow) = style.glow {
        for (grow, alpha) in [(14.0, 14), (9.0, 22), (5.0, 34)] {
            composite_shape(
                pixmap,
                layer,
                path,
                radius + grow * s,
                &solid(glow.with_alpha(255)),
                alpha,
            );
        }
    }
    let inner = radius - BORDER_WIDTH * s;

    // Kenarlık yalnızca bir halka olmalı: tam şekli çiz, içini sil, sonra birleştir.
    draw_shape(layer, path, radius, &solid(style.border.with_alpha(255)), true);
    let mut eraser = solid(theme::WHITE);
    eraser.blend_mode = BlendMode::Clear;
    draw_shape(layer, path, inner, &eraser, false);
    composite_layer(pixmap, layer, style.border.3);

    match style.fill {
        Fill::Solid(color) => composite_shape(pixmap, layer, path, inner, &solid(color.with_alpha(255)), color.3),
        Fill::Accent(alpha) => {
            // `bg-gradient-to-tr from-blue-600 to-indigo-600`: sol alttan sağ üste.
            let shader = LinearGradient::new(
                Point::from_xy(c - disc_r, c + disc_r),
                Point::from_xy(c + disc_r, c - disc_r),
                vec![
                    GradientStop::new(0.0, color_of(theme::BLUE_600)),
                    GradientStop::new(1.0, color_of(theme::INDIGO_600)),
                ],
                SpreadMode::Pad,
                Transform::identity(),
            )
            .unwrap_or_else(|| Shader::SolidColor(color_of(theme::BLUE_600)));
            let paint = Paint {
                shader,
                anti_alias: true,
                ..Paint::default()
            };
            composite_shape(pixmap, layer, path, inner, &paint, alpha);
        }
    }
}

/// Şekli katmana opak çizer ve katmanı `alpha` opaklığıyla ana tuvale bindirir.
fn composite_shape(pixmap: &mut Pixmap, layer: &mut Pixmap, path: &Path, radius: f32, paint: &Paint, alpha: u8) {
    draw_shape(layer, path, radius, paint, true);
    composite_layer(pixmap, layer, alpha);
}

/// `path` dolgusu + `radius` kalınlıkta yuvarlak kontur = köşeleri yuvarlatılmış şekil.
fn draw_shape(layer: &mut Pixmap, path: &Path, radius: f32, paint: &Paint, clear_first: bool) {
    if clear_first {
        layer.fill(Color::TRANSPARENT);
    }
    layer.fill_path(path, paint, FillRule::Winding, Transform::identity(), None);
    let stroke = Stroke {
        width: 2.0 * radius.max(0.0),
        line_join: LineJoin::Round,
        line_cap: LineCap::Round,
        ..Stroke::default()
    };
    layer.stroke_path(path, paint, &stroke, Transform::identity(), None);
}

fn composite_layer(pixmap: &mut Pixmap, layer: &Pixmap, alpha: u8) {
    let layer_paint = PixmapPaint {
        opacity: f32::from(alpha) / 255.0,
        ..PixmapPaint::default()
    };
    pixmap.draw_pixmap(0, 0, layer.as_ref(), &layer_paint, Transform::identity(), None);
}

/// Metni `center` noktasına yatay ve dikey ortalanmış olarak çizer.
fn draw_text(pixmap: &mut Pixmap, font: &FontVec, text: &str, size: f32, center: (f32, f32), color: Rgba) {
    let scale = PxScale::from(size);
    let scaled = font.as_scaled(scale);

    let mut glyphs: Vec<Glyph> = Vec::with_capacity(text.len());
    let mut caret = 0.0_f32;
    let mut prev = None;
    for ch in text.chars() {
        let id = scaled.glyph_id(ch);
        if let Some(p) = prev {
            caret += scaled.kern(p, id);
        }
        glyphs.push(id.with_scale_and_position(scale, point(caret, 0.0)));
        caret += scaled.h_advance(id);
        prev = Some(id);
    }

    // Büyük harf yüksekliğine göre dikey ortalama, x-height'tan daha dengeli durur.
    let cap_height = scaled.ascent() * 0.70;
    let origin = point(center.0 - caret / 2.0, center.1 + cap_height / 2.0);
    let (width, height) = (pixmap.width() as i32, pixmap.height() as i32);
    let data = pixmap.data_mut();

    for mut glyph in glyphs {
        glyph.position = point(glyph.position.x + origin.x, glyph.position.y + origin.y);
        let Some(outlined) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outlined.px_bounds();
        outlined.draw(|gx, gy, coverage| {
            let x = bounds.min.x as i32 + gx as i32;
            let y = bounds.min.y as i32 + gy as i32;
            if x >= 0 && y >= 0 && x < width && y < height {
                let i = ((y * width + x) * 4) as usize;
                blend_premultiplied(&mut data[i..i + 4], color, coverage);
            }
        });
    }
}

/// Kaynak-üstünde (source-over) karıştırma; hedef ön-çarpılmış RGBA.
fn blend_premultiplied(dst: &mut [u8], color: Rgba, coverage: f32) {
    let a = f32::from(color.3) / 255.0 * coverage.clamp(0.0, 1.0);
    let inv = 1.0 - a;
    for (d, s) in dst[..3].iter_mut().zip([color.0, color.1, color.2]) {
        *d = (f32::from(s) * a + f32::from(*d) * inv).round() as u8;
    }
    dst[3] = (255.0 * a + f32::from(dst[3]) * inv).round() as u8;
}

fn color_of(color: Rgba) -> Color {
    Color::from_rgba8(color.0, color.1, color.2, color.3)
}

fn solid(color: Rgba) -> Paint<'static> {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color.0, color.1, color.2, color.3);
    paint.anti_alias = true;
    paint
}

fn fill_path(pixmap: &mut Pixmap, path: &Path, color: Rgba) {
    pixmap.fill_path(path, &solid(color), FillRule::Winding, Transform::identity(), None);
}

fn stroke_path(pixmap: &mut Pixmap, path: &Path, color: Rgba, width: f32) {
    let stroke = Stroke {
        width,
        ..Stroke::default()
    };
    pixmap.stroke_path(path, &solid(color), &stroke, Transform::identity(), None);
}

fn circle(cx: f32, cy: f32, r: f32) -> Path {
    PathBuilder::from_circle(cx, cy, r).expect("yarıçap pozitif olmalı")
}

/// Köşe yuvarlaklığı kadar içeri çekilmiş halka dilimi. Kenarlar, dilimler
/// arasında sabit genişlikte boşluk kalacak şekilde paralel kaydırılır.
fn petal_path(geo: Geometry, i: usize) -> Path {
    const STEPS: usize = 28;
    let s = geo.scale;
    let c = geo.center();
    let k = Geometry::ROUNDING * s;
    let offset = Geometry::GAP / 2.0 * s + k;
    let r_out = Geometry::RING_OUT * s - k;
    let r_in = Geometry::RING_IN * s + k;

    let mid = Geometry::segment_angle(i);
    let half = Geometry::segment_half_width();
    let d_out = (offset / r_out).asin();
    let d_in = (offset / r_in).asin();

    let mut pb = PathBuilder::new();
    let (a0, a1) = (mid - half + d_out, mid + half - d_out);
    for step in 0..=STEPS {
        let a = a0 + (a1 - a0) * step as f32 / STEPS as f32;
        let (x, y) = (c + r_out * a.cos(), c + r_out * a.sin());
        if step == 0 {
            pb.move_to(x, y);
        } else {
            pb.line_to(x, y);
        }
    }
    let (b0, b1) = (mid + half - d_in, mid - half + d_in);
    for step in 0..=STEPS {
        let a = b0 + (b1 - b0) * step as f32 / STEPS as f32;
        pb.line_to(c + r_in * a.cos(), c + r_in * a.sin());
    }
    pb.close();
    pb.finish().expect("dilim yolu boş olamaz")
}

/// README ve önizlemeler için çarkı referans sayfanın arka planı üzerine oturtur.
#[cfg(test)]
fn on_page_background(wheel: &Pixmap) -> Pixmap {
    let (w, h) = (wheel.width(), wheel.height());
    let mut out = Pixmap::new(w, h).unwrap();
    let shader = RadialGradient::new(
        Point::from_xy(w as f32 / 2.0, 0.0),
        0.0,
        Point::from_xy(w as f32 / 2.0, 0.0),
        h as f32 * 1.1,
        vec![
            GradientStop::new(0.0, color_of(theme::BG_TOP)),
            GradientStop::new(1.0, color_of(theme::BG_BOTTOM)),
        ],
        SpreadMode::Pad,
        Transform::identity(),
    )
    .unwrap();
    let paint = Paint {
        shader,
        ..Paint::default()
    };
    out.fill_rect(
        tiny_skia::Rect::from_xywh(0.0, 0.0, w as f32, h as f32).unwrap(),
        &paint,
        Transform::identity(),
        None,
    );
    out.draw_pixmap(
        0,
        0,
        wheel.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tasarımı gözle kontrol etmek için: `cargo test render_preview -- --ignored`
    /// `target/cark-*.png` (saydam) ve `assets/screenshots/wheel.png` (README) üretir.
    #[test]
    #[ignore = "önizleme PNG'si üretir"]
    fn render_preview() {
        let renderer = Renderer::new();
        let geo = Geometry { scale: 1.0 };
        let base = WheelModel {
            phase: Phase::Open,
            files: vec![PathBuf::from("mandalina.png")],
            total_bytes: 2_300_000,
            hover: None,
        };
        let cases = [
            ("bos", base.clone()),
            (
                "webp",
                WheelModel {
                    hover: Some(Hit::Segment(1)),
                    ..base.clone()
                },
            ),
            (
                "merkez",
                WheelModel {
                    hover: Some(Hit::Center),
                    ..base.clone()
                },
            ),
            (
                "pdf-girdisi",
                WheelModel {
                    files: vec![PathBuf::from("belge.pdf")],
                    hover: Some(Hit::Segment(6)),
                    ..base.clone()
                },
            ),
            (
                "bitti",
                WheelModel {
                    phase: Phase::Done {
                        text: "840,2 KB".into(),
                        ok: true,
                    },
                    hover: Some(Hit::Segment(1)),
                    ..base.clone()
                },
            ),
        ];
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for (name, model) in cases {
            let wheel = renderer.render(geo, &model);
            wheel
                .save_png(root.join("target").join(format!("cark-{name}.png")))
                .unwrap();
            on_page_background(&wheel)
                .save_png(root.join("target").join(format!("cark-{name}-zemin.png")))
                .unwrap();
        }

        let screenshots = root.join("assets").join("screenshots");
        std::fs::create_dir_all(&screenshots).unwrap();
        let hero = renderer.render(
            geo,
            &WheelModel {
                hover: Some(Hit::Segment(1)),
                ..base
            },
        );
        on_page_background(&hero)
            .save_png(screenshots.join("wheel.png"))
            .unwrap();
    }
}
