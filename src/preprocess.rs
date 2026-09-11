use std::collections::{HashMap, VecDeque};
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use image::ImageDecoder;
use image::{DynamicImage, ImageBuffer, ImageReader, Rgba, RgbaImage};

use crate::color::Rgb;
use crate::Config;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    Nearest,
    Cubic,
    Lanczos,
}

#[derive(Debug, Clone)]
pub struct PreparedImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<Option<Rgb>>,
    pub rgba: RgbaImage,
    pub palette: Vec<Rgb>,
}

impl PreparedImage {
    pub fn at(&self, x: u32, y: u32) -> Option<Rgb> {
        self.pixels[(y * self.width + x) as usize]
    }
}

pub fn load_and_prepare(config: &Config) -> Result<PreparedImage> {
    let reader = ImageReader::open(&config.input)
        .with_context(|| format!("opening {}", config.input.display()))?
        .with_guessed_format()
        .context("detecting image format")?;
    let mut decoder = reader.into_decoder().context("creating image decoder")?;
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder).context("decoding image")?;
    image.apply_orientation(orientation);
    let mut rgba = image.to_rgba8();
    if config.upscale > 1 {
        rgba = resize_premultiplied(&rgba, config.upscale, config.filter);
    }
    if let Some(bg) = config.remove_background {
        remove_background(&mut rgba, bg, config.background_tolerance);
    }

    let (w, h) = rgba.dimensions();
    let mut opaque = vec![false; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let a = rgba.get_pixel(x, y)[3];
            opaque[(y * w + x) as usize] = a > 0 && a >= config.alpha_threshold;
        }
    }
    if config.trim {
        if let Some((minx, miny, maxx, maxy)) = opaque_bounds(w, h, &opaque) {
            rgba = image::imageops::crop_imm(&rgba, minx, miny, maxx - minx + 1, maxy - miny + 1)
                .to_image();
        }
    }
    let (w, h) = rgba.dimensions();
    let (palette, pixels) = quantize(
        &rgba,
        config.alpha_threshold,
        config.palette.as_deref(),
        config.colors,
    );
    if pixels.iter().all(Option::is_none) {
        return Err(anyhow!(
            "artwork is empty after transparency/background processing"
        ));
    }
    Ok(PreparedImage {
        width: w,
        height: h,
        pixels,
        rgba,
        palette,
    })
}

fn resize_premultiplied(src: &RgbaImage, factor: u32, filter: Filter) -> RgbaImage {
    let mut premul = src.clone();
    for p in premul.pixels_mut() {
        let a = p[3] as f32 / 255.0;
        p[0] = (p[0] as f32 * a).round() as u8;
        p[1] = (p[1] as f32 * a).round() as u8;
        p[2] = (p[2] as f32 * a).round() as u8;
    }
    let kind = match filter {
        Filter::Nearest => image::imageops::FilterType::Nearest,
        Filter::Cubic => image::imageops::FilterType::CatmullRom,
        Filter::Lanczos => image::imageops::FilterType::Lanczos3,
    };
    let mut out =
        image::imageops::resize(&premul, src.width() * factor, src.height() * factor, kind);
    // Resize the premultiplied channels separately and then unpremultiply. This avoids
    // dark fringes when translucent pixels are interpolated over a transparent edge.
    for p in out.pixels_mut() {
        let a = p[3] as f32 / 255.0;
        if a > 0.001 {
            p[0] = (p[0] as f32 / a).round().clamp(0.0, 255.0) as u8;
            p[1] = (p[1] as f32 / a).round().clamp(0.0, 255.0) as u8;
            p[2] = (p[2] as f32 / a).round().clamp(0.0, 255.0) as u8;
        } else {
            p[0] = 0;
            p[1] = 0;
            p[2] = 0;
        }
    }
    out
}

fn remove_background(image: &mut RgbaImage, bg: Rgb, tolerance: u8) {
    let (w, h) = image.dimensions();
    let mut seen = vec![false; (w * h) as usize];
    let mut q = VecDeque::new();
    for x in 0..w {
        q.push_back((x, 0));
        if h > 1 {
            q.push_back((x, h - 1));
        }
    }
    for y in 0..h {
        q.push_back((0, y));
        if w > 1 {
            q.push_back((w - 1, y));
        }
    }
    while let Some((x, y)) = q.pop_front() {
        let i = (y * w + x) as usize;
        if seen[i] {
            continue;
        }
        seen[i] = true;
        let p = image.get_pixel(x, y);
        let c = Rgb(p[0], p[1], p[2]);
        if p[3] == 0 || c.distance(bg) > tolerance {
            continue;
        }
        image.get_pixel_mut(x, y)[3] = 0;
        if x > 0 {
            q.push_back((x - 1, y));
        }
        if x + 1 < w {
            q.push_back((x + 1, y));
        }
        if y > 0 {
            q.push_back((x, y - 1));
        }
        if y + 1 < h {
            q.push_back((x, y + 1));
        }
    }
}

fn opaque_bounds(w: u32, h: u32, mask: &[bool]) -> Option<(u32, u32, u32, u32)> {
    let mut minx = w;
    let mut miny = h;
    let mut maxx = 0;
    let mut maxy = 0;
    let mut any = false;
    for y in 0..h {
        for x in 0..w {
            if mask[(y * w + x) as usize] {
                any = true;
                minx = minx.min(x);
                maxx = maxx.max(x);
                miny = miny.min(y);
                maxy = maxy.max(y);
            }
        }
    }
    if any {
        Some((minx, miny, maxx, maxy))
    } else {
        None
    }
}

fn quantize(
    image: &RgbaImage,
    alpha: u8,
    requested: Option<&[Rgb]>,
    max_colors: usize,
) -> (Vec<Rgb>, Vec<Option<Rgb>>) {
    let mut counts: HashMap<Rgb, usize> = HashMap::new();
    for p in image.pixels() {
        if p[3] > 0 && p[3] >= alpha {
            *counts.entry(Rgb(p[0], p[1], p[2])).or_default() += 1;
        }
    }
    let mut palette = if let Some(p) = requested {
        p.to_vec()
    } else {
        let mut v: Vec<_> = counts.iter().map(|(c, n)| (*c, *n)).collect();
        v.sort_by_key(|(color, count)| (std::cmp::Reverse(*count), *color));
        // JPEG/WebP artwork commonly has many compression shades around one
        // intended flat color. Spend the palette budget on distinct
        // representatives instead of those near-duplicates. The threshold is
        // intentionally a little wider than ordinary JPEG ringing so a flat
        // sign background stays one region.
        let total: usize = v.iter().map(|(_, count)| *count).sum();
        let significant_count = total / 20; // 5% of visible pixels
        let mut selected = Vec::new();
        for (color, count) in v {
            if selected.len() >= 2 && count < significant_count {
                continue;
            }
            if selected
                .iter()
                .all(|chosen: &Rgb| chosen.distance(color) >= 24)
            {
                selected.push(color);
                if selected.len() == max_colors {
                    break;
                }
            }
        }
        selected
    };
    if palette.is_empty() {
        palette.push(Rgb(0, 0, 0));
    }
    let mut unique = Vec::with_capacity(palette.len());
    for color in palette {
        if !unique.contains(&color) {
            unique.push(color);
        }
    }
    let palette = unique;
    let pixels = image
        .pixels()
        .map(|p| {
            if p[3] == 0 || p[3] < alpha {
                None
            } else {
                let c = Rgb(p[0], p[1], p[2]);
                Some(
                    palette
                        .iter()
                        .min_by_key(|q| q.distance(c))
                        .copied()
                        .unwrap(),
                )
            }
        })
        .collect();
    (palette, pixels)
}

pub fn write_preview(
    path: &Path,
    prepared: &PreparedImage,
    _parts: &[crate::mesh::Part],
) -> Result<()> {
    let mut image = ImageBuffer::new(prepared.width, prepared.height);
    for y in 0..prepared.height {
        for x in 0..prepared.width {
            let p = match prepared.at(x, y) {
                Some(c) => Rgba([c.0, c.1, c.2, 255]),
                None => Rgba([255, 255, 255, 0]),
            };
            image.put_pixel(x, y, p);
        }
    }
    image
        .save(path)
        .with_context(|| format!("saving preview {}", path.display()))?;
    Ok(())
}
