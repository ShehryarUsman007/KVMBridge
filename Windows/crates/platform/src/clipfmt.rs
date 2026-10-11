//! Converting clipboard content between what Windows keeps and what travels
//! between computers (UTF-8 text with `\n` line ends, an HTML fragment, PNG
//! images and `text/uri-list` file lists).

use std::io::Cursor;

/// Windows text (UTF-16, maybe NUL-terminated) to UTF-8 with `\n` line ends.
pub fn text_from_windows(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end]).replace("\r\n", "\n")
}

/// UTF-8 text to Windows text: `\r\n` line ends, UTF-16, NUL-terminated.
pub fn text_to_windows(text: &str) -> Vec<u16> {
    let crlf = text.replace("\r\n", "\n").replace('\n', "\r\n");
    crlf.encode_utf16().chain(std::iter::once(0)).collect()
}

fn header_value(header: &str, key: &str) -> Option<usize> {
    header.lines().find_map(|l| l.strip_prefix(key)).and_then(|v| v.trim().parse().ok())
}

/// The fragment of a Windows "HTML Format" clipboard entry.
pub fn html_fragment(data: &[u8]) -> Option<String> {
    let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
    let data = &data[..end];
    let head = String::from_utf8_lossy(&data[..data.len().min(512)]);
    let range = match (header_value(&head, "StartFragment:"), header_value(&head, "EndFragment:")) {
        (Some(a), Some(z)) => Some((a, z)),
        _ => match (header_value(&head, "StartHTML:"), header_value(&head, "EndHTML:")) {
            (Some(a), Some(z)) => Some((a, z)),
            _ => None,
        },
    };
    let (a, z) = range.filter(|&(a, z)| a <= z && z <= data.len())?;
    Some(String::from_utf8_lossy(&data[a..z]).into_owned())
}

/// The body of an HTML document (or the text itself if it has no body).
fn body_of(html: &str) -> &str {
    let lower = html.to_ascii_lowercase();
    let Some(open) = lower.find("<body") else { return html };
    let Some(gt) = lower[open..].find('>') else { return html };
    let start = open + gt + 1;
    let end = lower.rfind("</body>").filter(|&e| e >= start).unwrap_or(html.len());
    &html[start..end]
}

/// An HTML fragment (or document) as a Windows "HTML Format" entry.
pub fn html_to_windows(html: &str) -> Vec<u8> {
    let fragment = body_of(html);
    let pre = "<html><body>\r\n<!--StartFragment-->";
    let post = "<!--EndFragment-->\r\n</body></html>";
    let header = |a: usize, b: usize, c: usize, d: usize| {
        format!("Version:0.9\r\nStartHTML:{a:010}\r\nEndHTML:{b:010}\r\nStartFragment:{c:010}\r\nEndFragment:{d:010}\r\n")
    };
    let start_html = header(0, 0, 0, 0).len();
    let start_fragment = start_html + pre.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + post.len();
    let mut out = header(start_html, end_html, start_fragment, end_fragment).into_bytes();
    out.extend_from_slice(pre.as_bytes());
    out.extend_from_slice(fragment.as_bytes());
    out.extend_from_slice(post.as_bytes());
    out.push(0);
    out
}

/// An RGBA image, 8 bits per channel, rows top to bottom.
#[derive(Debug, PartialEq, Eq)]
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

fn u16_at(b: &[u8], i: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(i..i + 2)?.try_into().ok()?))
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

const BI_RGB: u32 = 0;
const BI_BITFIELDS: u32 = 3;

/// Value of a colour channel under `mask` in a 32-bit pixel, scaled to 8 bits.
fn channel(px: u32, mask: u32) -> u8 {
    if mask == 0 {
        return 0;
    }
    let shift = mask.trailing_zeros();
    let bits = (mask >> shift).count_ones();
    let v = (px & mask) >> shift;
    if bits >= 8 {
        (v >> (bits - 8)) as u8
    } else {
        ((v * 255) / ((1 << bits) - 1)) as u8
    }
}

/// A Windows device independent bitmap (CF_DIB / CF_DIBV5: a header and the
/// pixels) as RGBA. Handles uncompressed 24 and 32 bit images.
pub fn dib_to_rgba(dib: &[u8]) -> Option<Rgba> {
    let header = u32_at(dib, 0)? as usize;
    if header < 40 {
        return None;
    }
    let width = i32::from_le_bytes(dib.get(4..8)?.try_into().ok()?);
    let height = i32::from_le_bytes(dib.get(8..12)?.try_into().ok()?);
    let bits = u16_at(dib, 14)?;
    let compression = u32_at(dib, 16)?;
    if width <= 0 || height == 0 || !matches!(bits, 24 | 32) || !matches!(compression, BI_RGB | BI_BITFIELDS) {
        return None;
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    let stride = (w * bits as usize / 8 + 3) & !3;
    let size = stride.checked_mul(h)?;
    // The pixels end the entry. (Colour masks may or may not follow a
    // version 5 header, so do not count on where they start.)
    if dib.len() < header + size {
        return None;
    }
    let pixels = &dib[dib.len() - size..];
    let (rm, gm, bm, am) = if compression == BI_BITFIELDS {
        let masks_at = if header >= 56 { 40 } else { header };
        let a = if header >= 56 { u32_at(dib, 52)? } else { 0 };
        (u32_at(dib, masks_at)?, u32_at(dib, masks_at + 4)?, u32_at(dib, masks_at + 8)?, a)
    } else if bits == 32 {
        (0x00FF_0000, 0x0000_FF00, 0x0000_00FF, 0xFF00_0000)
    } else {
        (0, 0, 0, 0)
    };
    let mut out = vec![0u8; w * h * 4];
    let mut any_alpha = false;
    for y in 0..h {
        // Positive height: the bottom row comes first.
        let src_row = if height > 0 { h - 1 - y } else { y };
        let row = &pixels[src_row * stride..];
        for x in 0..w {
            let o = (y * w + x) * 4;
            if bits == 24 {
                let p = &row[x * 3..x * 3 + 3];
                out[o..o + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
            } else {
                let px = u32::from_le_bytes(row[x * 4..x * 4 + 4].try_into().ok()?);
                let a = channel(px, am);
                any_alpha |= a != 0;
                out[o..o + 4].copy_from_slice(&[channel(px, rm), channel(px, gm), channel(px, bm), a]);
            }
        }
    }
    if bits == 32 && !any_alpha {
        // Most programs leave the fourth byte at zero: the image is opaque.
        out.as_chunks_mut::<4>().0.iter_mut().for_each(|p| p[3] = 255);
    }
    Some(Rgba { width: w as u32, height: h as u32, pixels: out })
}

/// An RGBA image as a CF_DIBV5 entry (32 bit, with alpha, bottom-up).
pub fn rgba_to_dibv5(img: &Rgba) -> Vec<u8> {
    let (w, h) = (img.width as usize, img.height as usize);
    let mut out = Vec::with_capacity(124 + w * h * 4);
    let mut put = |v: u32| out.extend_from_slice(&v.to_le_bytes());
    put(124); // bV5Size
    put(img.width); // bV5Width
    put(img.height); // bV5Height (positive: bottom-up)
    put(1 | (32 << 16)); // bV5Planes, bV5BitCount
    put(BI_BITFIELDS); // bV5Compression
    put((w * h * 4) as u32); // bV5SizeImage
    put(2835); // 72 dpi
    put(2835);
    put(0); // bV5ClrUsed
    put(0); // bV5ClrImportant
    put(0x00FF_0000); // red mask
    put(0x0000_FF00); // green mask
    put(0x0000_00FF); // blue mask
    put(0xFF00_0000); // alpha mask
    put(0x7352_4742); // LCS_sRGB
    for _ in 0..12 {
        put(0); // endpoints (9) and gammas (3)
    }
    put(4); // LCS_GM_IMAGES
    put(0); // profile data
    put(0); // profile size
    put(0); // reserved
    for y in (0..h).rev() {
        for p in img.pixels[y * w * 4..(y + 1) * w * 4].as_chunks::<4>().0 {
            out.extend_from_slice(&[p[2], p[1], p[0], p[3]]);
        }
    }
    out
}

pub fn png_encode(img: &Rgba) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, img.width, img.height);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().ok()?;
    writer.write_image_data(&img.pixels).ok()?;
    writer.finish().ok()?;
    Some(out)
}

pub fn png_decode(data: &[u8]) -> Option<Rgba> {
    let mut dec = png::Decoder::new(Cursor::new(data));
    dec.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = dec.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    let buf = &buf[..info.buffer_size()];
    let (width, height) = (info.width, info.height);
    let pixels: Vec<u8> = match info.color_type {
        png::ColorType::Rgba => buf.to_vec(),
        png::ColorType::Rgb => buf.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => buf.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => buf.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        png::ColorType::Indexed => return None,
    };
    (pixels.len() == width as usize * height as usize * 4).then_some(Rgba { width, height, pixels })
}

/// A list of files as a CF_HDROP entry (DROPFILES with wide paths).
pub fn hdrop(paths: &[std::path::PathBuf]) -> Vec<u8> {
    let mut out = Vec::new();
    for v in [20u32, 0, 0, 0, 1] {
        // pFiles, pt.x, pt.y, fNC, fWide
        out.extend_from_slice(&v.to_le_bytes());
    }
    for p in paths {
        for u in p.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)) {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
    out.extend_from_slice(&[0, 0]);
    out
}
