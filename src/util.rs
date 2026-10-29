//! Küçük biçimlendirme ve platform yardımcıları.

use std::path::Path;
use std::process::Command;

/// Türkçe biçimde bayt gösterimi: `1536` -> "1,5 KB".
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit]).replace('.', ",")
    }
}

/// `part / whole` oranını Türkçe yüzde biçiminde verir: "%61".
pub fn format_percent(part: u64, whole: u64) -> String {
    if whole == 0 {
        return "%0".into();
    }
    format!("%{:.0}", part as f64 * 100.0 / whole as f64)
}

/// Klasörü işletim sisteminin dosya yöneticisinde açar.
pub fn open_in_file_manager(path: &Path) -> std::io::Result<()> {
    let program = if cfg!(target_os = "windows") {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    // Explorer başarılı olduğunda bile 1 döndürebildiği için çıkış koduna bakılmaz.
    Command::new(program).arg(path).spawn().map(drop)
}

/// Bağlantıyı varsayılan tarayıcıda açar.
pub fn open_url(url: &str) -> std::io::Result<()> {
    if cfg!(target_os = "windows") {
        // `start` cmd'nin iç komutudur; ilk tırnaklı argüman pencere başlığıdır.
        Command::new("cmd").args(["/C", "start", "", url]).spawn().map(drop)
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn().map(drop)
    } else {
        Command::new("xdg-open").arg(url).spawn().map(drop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes_in_turkish() {
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1,5 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5,0 MB");
    }

    #[test]
    fn formats_percent() {
        assert_eq!(format_percent(61, 100), "%61");
        assert_eq!(format_percent(1, 0), "%0");
    }
}
