//! 剪贴板图片：截图和复制的图片存成本机 PNG，可再次复制，也可用 Windows 自带的文字识别取出文字。
//!
//! - 默认不记录，需在设置里打开「记录图片」。
//! - 图片保存在配置目录的 `clipboard-images/`，记录删除或被挤掉时文件一起删除。
//! - 文字识别用系统的 `Windows.Media.Ocr`，在本机完成，不联网。

use std::path::{Path, PathBuf};

/// 超过这个像素数的图片不记录（约 8K×5K）。
pub const MAX_PIXELS: u64 = 40_000_000;
/// 缩略图最长边。
pub const THUMB: u32 = 240;
pub const DIR: &str = "clipboard-images";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClipImage {
    /// `clipboard-images/` 下的文件名：内容哈希 + `.png`。
    pub file: String,
    pub w: u32,
    pub h: u32,
}

impl ClipImage {
    /// 只接受自己生成的文件名，防止改过的 clipboard.json 指向别处。
    pub fn valid_name(file: &str) -> bool {
        file.strip_suffix(".png").is_some_and(|stem| stem.len() == 16 && stem.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()))
    }
    pub fn path(&self, dir: &Path) -> PathBuf { dir.join(&self.file) }
    pub fn thumb_path(&self, dir: &Path) -> PathBuf { dir.join(self.file.replace(".png", ".thumb.png")) }
}

/// 解码后的图片，RGBA 每像素 4 字节，自上而下。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rgba {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

fn fnv(parts: &[&[u8]]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for part in parts {
        for &b in *part { hash = (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3); }
    }
    hash
}

fn u16_at(b: &[u8], o: usize) -> Option<u16> { Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?)) }
fn u32_at(b: &[u8], o: usize) -> Option<u32> { Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?)) }

fn channel(v: u32, mask: u32) -> u8 {
    if mask == 0 { return 0; }
    let bits = mask.count_ones();
    let value = (v & mask) >> mask.trailing_zeros();
    if bits >= 8 { (value >> (bits - 8)) as u8 } else { (value * 255 / ((1 << bits) - 1)) as u8 }
}

/// 解析剪贴板里的 `CF_DIB`（不带文件头的 BMP）；支持 24/32 位、BI_RGB 与 BI_BITFIELDS，自下而上或自上而下。
pub fn decode_dib(dib: &[u8]) -> Option<Rgba> {
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;
    const BI_ALPHABITFIELDS: u32 = 6;
    let header = u32_at(dib, 0)? as usize;
    if header < 40 || header > dib.len() { return None; }
    let width = u32_at(dib, 4)? as i32;
    let height = u32_at(dib, 8)? as i32;
    let bpp = u16_at(dib, 14)?;
    let compression = u32_at(dib, 16)?;
    let palette = u32_at(dib, 32)? as usize;
    if width <= 0 || height == 0 || height == i32::MIN || !matches!(bpp, 24 | 32) { return None; }
    let (w, h, top_down) = (width as u32, height.unsigned_abs(), height < 0);
    if w as u64 * h as u64 > MAX_PIXELS { return None; }
    let (mut masks, mut extra) = ([0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0], 0usize);
    match compression {
        BI_RGB => {}
        BI_BITFIELDS | BI_ALPHABITFIELDS if bpp == 32 => {
            let count = if compression == BI_ALPHABITFIELDS { 4 } else { 3 };
            // V4/V5 头里自带掩码；普通 40 字节头的掩码紧跟在头后面。
            let at = 40;
            if header < 52 { extra = count * 4; }
            for (i, mask) in masks.iter_mut().enumerate().take(if header >= 56 { 4 } else { count }) { *mask = u32_at(dib, at + i * 4)?; }
        }
        _ => return None,
    }
    let offset = header.checked_add(extra)?.checked_add(palette.checked_mul(4)?)?;
    let stride = ((w as usize * bpp as usize + 31) / 32) * 4;
    let end = offset.checked_add(stride.checked_mul(h as usize)?)?;
    let pixels = dib.get(offset..end)?;
    let mut px = vec![0u8; w as usize * h as usize * 4];
    for y in 0..h as usize {
        let src = &pixels[(if top_down { y } else { h as usize - 1 - y }) * stride..][..stride];
        let out = &mut px[y * w as usize * 4..][..w as usize * 4];
        for x in 0..w as usize {
            let o = &mut out[x * 4..x * 4 + 4];
            if bpp == 24 {
                o.copy_from_slice(&[src[x * 3 + 2], src[x * 3 + 1], src[x * 3], 255]);
            } else {
                let v = u32::from_le_bytes(src[x * 4..x * 4 + 4].try_into().ok()?);
                o.copy_from_slice(&[channel(v, masks[0]), channel(v, masks[1]), channel(v, masks[2]), if masks[3] == 0 { 255 } else { channel(v, masks[3]) }]);
            }
        }
    }
    // 不少程序把 alpha 全写成 0，按不透明处理。
    if masks[3] != 0 && px.chunks_exact(4).all(|p| p[3] == 0) {
        px.chunks_exact_mut(4).for_each(|p| p[3] = 255);
    }
    Some(Rgba { w, h, px })
}

/// 生成 `CF_DIB`：40 字节头 + 32 位 BGRA，自下而上。
pub fn encode_dib(img: &Rgba) -> Vec<u8> {
    let row = img.w as usize * 4;
    let mut out = Vec::with_capacity(40 + row * img.h as usize);
    for v in [40u32, img.w, img.h] { out.extend_from_slice(&v.to_le_bytes()); }
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    for v in [0u32, (row * img.h as usize) as u32, 0, 0, 0, 0] { out.extend_from_slice(&v.to_le_bytes()); }
    for y in (0..img.h as usize).rev() {
        for p in img.px[y * row..][..row].chunks_exact(4) { out.extend_from_slice(&[p[2], p[1], p[0], p[3]]); }
    }
    out
}

pub fn decode_png(bytes: &[u8]) -> Option<Rgba> {
    let img = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).ok()?;
    if img.width() as u64 * img.height() as u64 > MAX_PIXELS { return None; }
    let img = img.to_rgba8();
    Some(Rgba { w: img.width(), h: img.height(), px: img.into_raw() })
}

fn encode_png(w: u32, h: u32, px: &[u8]) -> Option<Vec<u8>> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::png::PngEncoder::new(&mut out).write_image(px, w, h, image::ExtendedColorType::Rgba8).ok()?;
    Some(out)
}

/// 存成 `哈希.png` 与缩略图；同一张图只存一份。
pub fn store(img: &Rgba, dir: &Path) -> Option<ClipImage> {
    if img.w == 0 || img.h == 0 || img.px.len() != img.w as usize * img.h as usize * 4 { return None; }
    let hash = fnv(&[&img.w.to_le_bytes(), &img.h.to_le_bytes(), &img.px]);
    let clip = ClipImage { file: format!("{hash:016x}.png"), w: img.w, h: img.h };
    std::fs::create_dir_all(dir).ok()?;
    let path = clip.path(dir);
    let thumb = clip.thumb_path(dir);
    // 已有的文件刷新修改时间，免得清理时被当成没人用的旧文件。
    let touch = |p: &Path| std::fs::File::options().write(true).open(p).and_then(|f| f.set_modified(std::time::SystemTime::now())).is_ok();
    if !(path.exists() && touch(&path)) { crate::model::atomic_write(&path, &encode_png(img.w, img.h, &img.px)?).ok()?; }
    if !(thumb.exists() && touch(&thumb)) {
        let full = image::RgbaImage::from_raw(img.w, img.h, img.px.clone())?;
        let scale = (THUMB as f32 / img.w.max(img.h) as f32).min(1.0);
        let (tw, th) = (((img.w as f32 * scale).round() as u32).max(1), ((img.h as f32 * scale).round() as u32).max(1));
        let small = if scale < 1.0 { image::imageops::thumbnail(&full, tw, th) } else { full };
        crate::model::atomic_write(&thumb, &encode_png(small.width(), small.height(), small.as_raw())?).ok()?;
    }
    Some(clip)
}

fn cjk(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF | 0x20000..=0x2FA1F)
}

/// 把 OCR 的单词拼回一行：中日韩文字之间不加空格，其余用空格隔开。
pub fn join_words(words: &[String]) -> String {
    let mut out = String::new();
    for word in words.iter().map(|w| w.trim()).filter(|w| !w.is_empty()) {
        let joined = out.chars().last().is_some_and(cjk) && word.chars().next().is_some_and(cjk);
        if !out.is_empty() && !joined { out.push(' '); }
        out.push_str(word);
    }
    out
}

pub fn join_lines(lines: &[Vec<String>]) -> String {
    lines.iter().map(|l| join_words(l)).filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n")
}

/// 识别图片里的文字；耗时较长，放在后台线程调用。
#[cfg(windows)]
pub fn recognize(path: &Path) -> Result<String, String> {
    use windows::Graphics::Imaging::{BitmapPixelFormat, SoftwareBitmap};
    use windows::Media::Ocr::OcrEngine;
    use windows::Storage::Streams::DataWriter;
    let bytes = std::fs::read(path).map_err(|_| "The image file was deleted".to_string())?;
    let img = decode_png(&bytes).ok_or("Can't read the image")?;
    unsafe { let _ = windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED); }
    let engine = OcrEngine::TryCreateFromUserProfileLanguages().map_err(|_| "No text recognition language is installed; add Optical character recognition for Chinese or English in Windows Settings → Language".to_string())?;
    let max = OcrEngine::MaxImageDimension().unwrap_or(10_000).max(1);
    // 小截图放大一倍更容易认出小字；超过系统上限的按比例缩小。
    let scale = if img.w.max(img.h) > max { max as f32 / img.w.max(img.h) as f32 } else if img.w.max(img.h) * 2 <= max && (img.w as u64 * img.h as u64) < 2_000_000 { 2.0 } else { 1.0 };
    let (w, h, mut px) = if scale == 1.0 { (img.w, img.h, img.px) } else {
        let full = image::RgbaImage::from_raw(img.w, img.h, img.px).ok_or("Can't read the image")?;
        let (w, h) = (((img.w as f32 * scale) as u32).clamp(1, max), ((img.h as f32 * scale) as u32).clamp(1, max));
        (w, h, image::imageops::resize(&full, w, h, image::imageops::FilterType::Triangle).into_raw())
    };
    px.chunks_exact_mut(4).for_each(|p| p.swap(0, 2));
    let fail = |e: windows::core::Error| format!("Text recognition failed: {}", e.message());
    let writer = DataWriter::new().map_err(fail)?;
    writer.WriteBytes(&px).map_err(fail)?;
    let buffer = writer.DetachBuffer().map_err(fail)?;
    let bitmap = SoftwareBitmap::CreateCopyFromBuffer(&buffer, BitmapPixelFormat::Bgra8, w as i32, h as i32).map_err(fail)?;
    let result = engine.RecognizeAsync(&bitmap).map_err(fail)?.get().map_err(fail)?;
    let lines = result.Lines().map_err(fail)?;
    let mut out = Vec::new();
    for i in 0..lines.Size().unwrap_or(0) {
        let Ok(line) = lines.GetAt(i) else { continue };
        let Ok(words) = line.Words() else { continue };
        out.push((0..words.Size().unwrap_or(0)).filter_map(|j| words.GetAt(j).ok()?.Text().ok().map(|t| t.to_string())).collect());
    }
    Ok(join_lines(&out))
}

#[cfg(not(windows))]
pub fn recognize(_path: &Path) -> Result<String, String> { Err("Text recognition is Windows-only".into()) }

/// 把记录的图片放回剪贴板：`CF_DIB` 给大多数程序，`PNG` 保留透明度，另加岛仔标记避免重复记录。
#[cfg(windows)]
pub fn copy(path: &Path, marker: &str) -> bool {
    use clipboard_win::{raw, register_format, Clipboard};
    const CF_DIB: u32 = 8;
    let Ok(bytes) = std::fs::read(path) else { return false };
    let Some(img) = decode_png(&bytes) else { return false };
    let dib = encode_dib(&img);
    let Some(mark) = register_format(marker) else { return false };
    let Ok(_clipboard) = Clipboard::new_attempts(10) else { return false };
    if raw::set(CF_DIB, &dib).is_err() { return false; }
    if let Some(png) = register_format("PNG") { let _ = raw::set_without_clear(png.get(), &bytes); }
    raw::set_without_clear(mark.get(), &[1]).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Rgba {
        // 3×2：红 绿 蓝 / 白 黑 半透明灰
        Rgba { w: 3, h: 2, px: vec![255,0,0,255, 0,255,0,255, 0,0,255,255, 255,255,255,255, 0,0,0,255, 128,128,128,128] }
    }

    #[test]
    fn dib_round_trip_keeps_orientation_and_alpha() {
        let img = sample();
        let dib = encode_dib(&img);
        assert_eq!(dib.len(), 40 + 3 * 2 * 4);
        // BI_RGB 32 位：alpha 字节不可信，读回按不透明处理。
        let back = decode_dib(&dib).unwrap();
        assert_eq!((back.w, back.h), (3, 2));
        assert_eq!(&back.px[..12], &img.px[..12], "第一行仍在上面");
        assert_eq!(&back.px[20..24], &[128, 128, 128, 255]);
    }

    #[test]
    fn dib_reads_24_bit_and_bitfields_with_alpha() {
        // 24 位、自上而下（高度为负）、每行补齐到 4 字节。
        let mut dib = Vec::new();
        for v in [40u32, 1, (-2i32) as u32] { dib.extend_from_slice(&v.to_le_bytes()); }
        dib.extend_from_slice(&1u16.to_le_bytes()); dib.extend_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0; 24]);
        dib.extend_from_slice(&[10, 20, 30, 0, 40, 50, 60, 0]);
        let img = decode_dib(&dib).unwrap();
        assert_eq!(img.px, vec![30, 20, 10, 255, 60, 50, 40, 255]);

        // 32 位 BI_BITFIELDS，40 字节头后跟 RGB 掩码；alpha 掩码来自 V5 头时才生效。
        let mut dib = Vec::new();
        for v in [56u32, 1, 1] { dib.extend_from_slice(&v.to_le_bytes()); }
        dib.extend_from_slice(&1u16.to_le_bytes()); dib.extend_from_slice(&32u16.to_le_bytes());
        for v in [3u32, 4, 0, 0, 0, 0, 0x00ff_0000, 0x0000_ff00, 0x0000_00ff, 0xff00_0000] { dib.extend_from_slice(&v.to_le_bytes()); }
        dib.extend_from_slice(&[1, 2, 3, 77]);
        assert_eq!(decode_dib(&dib).unwrap().px, vec![3, 2, 1, 77]);
    }

    #[test]
    fn dib_rejects_truncated_or_unsupported_data() {
        let mut dib = encode_dib(&sample());
        dib.truncate(dib.len() - 1);
        assert!(decode_dib(&dib).is_none());
        let mut paletted = encode_dib(&sample());
        paletted[14] = 8;
        assert!(decode_dib(&paletted).is_none());
        assert!(decode_dib(&[0; 10]).is_none());
    }

    #[test]
    fn store_writes_png_and_thumbnail_once() {
        let dir = std::env::temp_dir().join(format!("daozai-clipimg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let big = Rgba { w: 600, h: 300, px: vec![200; 600 * 300 * 4] };
        let clip = store(&big, &dir).unwrap();
        assert!(ClipImage::valid_name(&clip.file));
        assert_eq!((clip.w, clip.h), (600, 300));
        let back = decode_png(&std::fs::read(clip.path(&dir)).unwrap()).unwrap();
        assert_eq!(back, big);
        let thumb = decode_png(&std::fs::read(clip.thumb_path(&dir)).unwrap()).unwrap();
        assert_eq!((thumb.w, thumb.h), (THUMB, THUMB / 2));
        assert_eq!(store(&big, &dir).unwrap(), clip, "同一张图同名");
        assert_ne!(store(&sample(), &dir).unwrap().file, clip.file);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn names_and_words() {
        assert!(ClipImage::valid_name("0123456789abcdef.png"));
        assert!(!ClipImage::valid_name("../../x.png") && !ClipImage::valid_name("0123456789ABCDEF.png") && !ClipImage::valid_name("0123456789abcdef.thumb.png"));
        let words = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(join_words(&words("你 好 ， 世 界")), "你好，世界");
        assert_eq!(join_words(&words("版 本 0.6 Windows 设 置")), "版本 0.6 Windows 设置");
        assert_eq!(join_lines(&[words("hello world"), vec![], words("第 二 行")]), "hello world\n第二行");
    }
}
