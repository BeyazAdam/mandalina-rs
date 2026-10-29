//! Windows: exe'ye simge ve yapımcı bilgisi (Özellikler → Ayrıntılar) gömer.
//!
//! `assets/icon.ico`, `cargo test icon_preview -- --ignored` ile üretilir.
//! Metinler ASCII tutulur: rc.exe UTF-8 kaynakları her ortamda doğru okumaz.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico")
            .set("ProductName", "Mandalina")
            .set("FileDescription", "Mandalina - offline image & PDF compressor")
            .set("CompanyName", "BeyazAdam")
            .set("LegalCopyright", "Copyright (c) 2026 BeyazAdam - MIT License")
            .set("InternalName", "mandalina")
            .set("OriginalFilename", "mandalina.exe");
        res.compile()
            .expect("Windows kaynakları (simge, sürüm bilgisi) derlenemedi");
    }
}
