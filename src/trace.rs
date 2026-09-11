use anyhow::{anyhow, Result};
use std::collections::{HashMap, VecDeque};

use crate::color::Rgb;
use crate::geometry::{signed_area, simplify_collinear, Contour, Point};
use crate::preprocess::PreparedImage;
use crate::{Config, Mode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TraceMode {
    Pixel,
    Polygon,
    Spline,
}

#[derive(Debug, Clone)]
pub struct Region {
    pub color: Rgb,
    pub color_index: usize,
    pub contours: Vec<Contour>,
}

#[derive(Debug, Clone)]
pub struct Traced {
    pub palette: Vec<Rgb>,
    pub regions: Vec<Region>,
    pub backing: Vec<Contour>,
    pub image_width: u32,
    pub image_height: u32,
    pub disconnected_pieces: usize,
}

type Key = (i32, i32);

pub fn trace(image: &mut PreparedImage, config: &Config) -> Result<Traced> {
    if image.width == 0 || image.height == 0 {
        return Err(anyhow!("image has no pixels"));
    }
    if let Some(limit) = config.speckle_area {
        cleanup_speckles(image, config, limit);
    }
    let trace_tolerance = match config.trace_mode {
        TraceMode::Pixel => 0.0,
        TraceMode::Polygon => config.simplify,
        // The pixel lattice already has exact shared edges.  Keep those edges
        // intact unless the caller explicitly requests simplification; this is
        // what guarantees identical coordinates at multi-colour junctions.
        TraceMode::Spline => config.simplify,
    };
    let mut regions = Vec::new();
    for (ci, &color) in image.palette.iter().enumerate() {
        let contours = trace_mask(
            image.width,
            image.height,
            |x, y| image.at(x, y).map(|c| c == color).unwrap_or(false),
            trace_tolerance,
        );
        if !contours.is_empty() {
            regions.push(Region {
                color,
                color_index: ci,
                contours,
            });
        }
    }
    if regions.is_empty() {
        return Err(anyhow!("no opaque artwork to trace"));
    }
    let backing = trace_mask(
        image.width,
        image.height,
        |x, y| image.at(x, y).is_some(),
        trace_tolerance,
    );
    let disconnected_pieces = backing.iter().filter(|c| signed_area(c) < 0.0).count();
    if config.mode == Mode::Backed && backing.is_empty() {
        return Err(anyhow!("backing union is empty"));
    }
    let used: std::collections::HashSet<Rgb> = regions.iter().map(|r| r.color).collect();
    let palette = image
        .palette
        .iter()
        .copied()
        .filter(|c| used.contains(c))
        .collect();
    Ok(Traced {
        palette,
        regions,
        backing,
        image_width: image.width,
        image_height: image.height,
        disconnected_pieces,
    })
}

fn cleanup_speckles(image: &mut PreparedImage, config: &Config, limit: f64) {
    if limit <= 0.0 {
        return;
    }
    let mm_per_px = config
        .width
        .map(|w| w / image.width as f64)
        .or_else(|| config.height.map(|h| h / image.height as f64))
        .unwrap_or(1.0);
    let max_pixels = (limit / (mm_per_px * mm_per_px)).max(1.0);
    let mut visited = vec![false; image.pixels.len()];
    for y in 0..image.height {
        for x in 0..image.width {
            let idx = (y * image.width + x) as usize;
            if visited[idx] || image.pixels[idx].is_none() {
                continue;
            }
            let color = image.pixels[idx].unwrap();
            let mut q = VecDeque::new();
            q.push_back((x, y));
            visited[idx] = true;
            let mut cells = Vec::new();
            let mut neighbors: HashMap<Rgb, usize> = HashMap::new();
            while let Some((cx, cy)) = q.pop_front() {
                cells.push((cx, cy));
                for (nx, ny) in [
                    (cx.wrapping_sub(1), cy),
                    (cx + 1, cy),
                    (cx, cy.wrapping_sub(1)),
                    (cx, cy + 1),
                ] {
                    if nx >= image.width || ny >= image.height {
                        continue;
                    }
                    let ni = (ny * image.width + nx) as usize;
                    match image.pixels[ni] {
                        Some(c) if c == color => {
                            if !visited[ni] {
                                visited[ni] = true;
                                q.push_back((nx, ny));
                            }
                        }
                        Some(c) => {
                            *neighbors.entry(c).or_default() += 1;
                        }
                        None => {}
                    }
                }
            }
            if cells.len() as f64 <= max_pixels {
                if let Some((&replacement, _)) = neighbors.iter().max_by_key(|(_, n)| **n) {
                    for (cx, cy) in cells {
                        image.pixels[(cy * image.width + cx) as usize] = Some(replacement);
                    }
                }
            }
        }
    }
}

fn trace_mask<F: Fn(u32, u32) -> bool>(w: u32, h: u32, inside: F, simplify: f64) -> Vec<Contour> {
    let mut outgoing: HashMap<Key, Vec<Key>> = HashMap::new();
    for y in 0..h {
        for x in 0..w {
            if !inside(x, y) {
                continue;
            }
            let x = x as i32;
            let y = y as i32;
            // Coordinates are in image space here.  They are flipped to model Y-up
            // while constructing the contour, so every neighboring color sees the
            // exact same boundary coordinates.
            if x == 0 || !inside((x - 1) as u32, y as u32) {
                add_edge(&mut outgoing, (x, y + 1), (x, y));
            }
            if y + 1 == h as i32 || !inside(x as u32, (y + 1) as u32) {
                add_edge(&mut outgoing, (x + 1, y + 1), (x, y + 1));
            }
            if x + 1 == w as i32 || !inside((x + 1) as u32, y as u32) {
                add_edge(&mut outgoing, (x + 1, y), (x + 1, y + 1));
            }
            if y == 0 || !inside(x as u32, (y - 1) as u32) {
                add_edge(&mut outgoing, (x, y), (x + 1, y));
            }
        }
    }
    let mut edges = Vec::new();
    for (s, ends) in outgoing {
        for e in ends {
            edges.push((s, e));
        }
    }
    let mut by_start: HashMap<Key, Vec<usize>> = HashMap::new();
    for (i, (s, _)) in edges.iter().enumerate() {
        by_start.entry(*s).or_default().push(i);
    }
    let mut used = vec![false; edges.len()];
    let mut contours = Vec::new();
    for i in 0..edges.len() {
        if used[i] {
            continue;
        }
        let (start, mut current) = edges[i];
        used[i] = true;
        let mut keys = vec![start, current];
        let mut previous = start;
        while current != start {
            let candidates = by_start
                .get(&current)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter(|j| !used[*j])
                .collect::<Vec<_>>();
            if candidates.is_empty() {
                break;
            }
            let next = *candidates
                .iter()
                .min_by_key(|j| turn_key(previous, current, edges[**j].1))
                .unwrap();
            used[next] = true;
            previous = current;
            current = edges[next].1;
            if current != start {
                keys.push(current);
            }
        }
        if keys.len() >= 3 && current == start {
            let mut c: Contour = keys
                .into_iter()
                .map(|(x, y)| Point::new(x as f64, (h as i32 - y) as f64))
                .collect();
            simplify_collinear(&mut c, 0.0);
            if c.len() >= 3 && signed_area(&c).abs() > 1e-8 {
                if simplify > 0.0 {
                    let reduced = crate::geometry::rdp_closed(&c, simplify);
                    if reduced.len() >= 3 {
                        c = reduced;
                    }
                }
                contours.push(c);
            }
        }
    }
    for contour in &mut contours {
        if let Some((start, _)) = contour.iter().enumerate().min_by(|(_, a), (_, b)| {
            a.x.partial_cmp(&b.x)
                .unwrap()
                .then_with(|| a.y.partial_cmp(&b.y).unwrap())
        }) {
            contour.rotate_left(start);
        }
    }
    contours.sort_by(|a, b| {
        let pa = a[0];
        let pb = b[0];
        pa.x.partial_cmp(&pb.x)
            .unwrap()
            .then_with(|| pa.y.partial_cmp(&pb.y).unwrap())
            .then_with(|| {
                signed_area(b)
                    .abs()
                    .partial_cmp(&signed_area(a).abs())
                    .unwrap()
            })
    });
    contours
}

fn add_edge(map: &mut HashMap<Key, Vec<Key>>, a: Key, b: Key) {
    map.entry(a).or_default().push(b);
}

fn turn_key(a: Key, b: Key, c: Key) -> (i32, i32, i32) {
    let ux = b.0 - a.0;
    let uy = b.1 - a.1;
    let vx = c.0 - b.0;
    let vy = c.1 - b.1;
    let cross = ux * vy - uy * vx;
    let dot = ux * vx + uy * vy; // straight first, then left turns, deterministic tie
    (if dot > 0 { 0 } else { 1 }, -cross, vx + vy)
}

/// Optional VTracer adapter for applications that want to inspect the same
/// prepared bitmap as a cutout SVG.  Keeping this behind a feature isolates
/// VTracer's prerelease API from the stable raster-to-mesh path.
#[cfg(feature = "vtracer-backend")]
pub fn vtracer_svg(image: &PreparedImage, config: &Config) -> Result<String> {
    let mut input = vtracer::ColorImage::new_w_h(image.width as usize, image.height as usize);
    for y in 0..image.height {
        for x in 0..image.width {
            let color = image.rgba.get_pixel(x, y);
            input.set_pixel(
                x as usize,
                y as usize,
                &vtracer::Color::new_rgba(color[0], color[1], color[2], color[3]),
            );
        }
    }
    let mut options = vtracer::Config {
        hierarchical: vtracer::Hierarchical::Cutout,
        mode: match config.trace_mode {
            TraceMode::Pixel => vtracer::FitMode::Pixel,
            TraceMode::Polygon => vtracer::FitMode::Polygon,
            TraceMode::Spline => vtracer::FitMode::Spline,
        },
        simplify: (config.simplify > 0.0).then_some(config.simplify),
        ..vtracer::Config::default()
    };
    if let Some(palette) = &config.palette {
        options.palette = palette
            .iter()
            .map(|c| vtracer::Color::new(c.0, c.1, c.2))
            .collect();
    } else {
        options.max_colors = Some(config.colors);
    }
    options
        .build()
        .map_err(|e| anyhow!("VTracer configuration failed: {e}"))?
        .to_svg(&input)
        .map_err(|e| anyhow!("VTracer tracing failed: {e}"))
}
