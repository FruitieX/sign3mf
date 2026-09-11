use crate::color::Rgb;
use crate::geometry::{bounds as contour_bounds, point_in_polygon, signed_area, Contour, Point};
use crate::trace::{Region, Traced};
use crate::{Config, Mode, PartsMode};
use anyhow::{anyhow, Result};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vertex {
    pub x: f64,
    pub y: f64,
    pub z: f64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Triangle {
    pub v1: u32,
    pub v2: u32,
    pub v3: u32,
}
#[derive(Debug, Clone, Default)]
pub struct Mesh {
    pub vertices: Vec<Vertex>,
    pub triangles: Vec<Triangle>,
}
#[derive(Debug, Clone)]
pub struct Part {
    pub name: String,
    pub color: Rgb,
    pub mesh: Mesh,
    pub is_backing: bool,
    pub region_index: usize,
}

pub fn build_parts(traced: &Traced, config: &Config) -> Result<Vec<Part>> {
    let all: Vec<Contour> = traced.backing.to_vec();
    let raw_bounds = contour_bounds(&all).ok_or_else(|| anyhow!("traced artwork has no bounds"))?;
    let source_w = (raw_bounds.2 - raw_bounds.0).max(1e-9);
    let source_h = (raw_bounds.3 - raw_bounds.1).max(1e-9);
    let requested = config.width.or(config.height).unwrap();
    let scale = if config.width.is_some() {
        requested / source_w
    } else {
        requested / source_h
    };
    let transform =
        |p: Point| Point::new((p.x - raw_bounds.0) * scale, (p.y - raw_bounds.1) * scale);
    let normalize = |cs: &[Contour]| -> Vec<Contour> {
        cs.iter()
            .map(|c| c.iter().copied().map(&transform).collect())
            .collect()
    };
    let depth = config.thickness;
    let mut parts = Vec::new();
    if config.mode == Mode::Backed {
        let backing_color = config.backing_color.unwrap_or_else(|| {
            traced
                .regions
                .iter()
                .max_by(|a, b| region_area(a).partial_cmp(&region_area(b)).unwrap())
                .map(|r| r.color)
                .unwrap_or(traced.palette[0])
        });
        let groups = group_contours(&normalize(&traced.backing));
        let mesh = extrude_groups(&groups, 0.0, depth - config.face_depth)?;
        parts.push(Part {
            name: format!("backing_{}", backing_color.hex()),
            color: backing_color,
            mesh,
            is_backing: true,
            region_index: usize::MAX,
        });
        for (ri, region) in traced.regions.iter().enumerate() {
            let groups = group_contours(&normalize(&region.contours));
            add_region_parts(
                &mut parts,
                region,
                ri,
                groups,
                depth - config.face_depth,
                depth,
                config.parts,
            )?;
        }
    } else {
        for (ri, region) in traced.regions.iter().enumerate() {
            let groups = group_contours(&normalize(&region.contours));
            add_region_parts(&mut parts, region, ri, groups, 0.0, depth, config.parts)?;
        }
    }
    Ok(parts)
}

fn region_area(region: &Region) -> f64 {
    region
        .contours
        .iter()
        .map(|c| -signed_area(c))
        .sum::<f64>()
        .abs()
}

fn add_region_parts(
    out: &mut Vec<Part>,
    region: &Region,
    ri: usize,
    groups: Vec<(Contour, Vec<Contour>)>,
    z0: f64,
    z1: f64,
    mode: PartsMode,
) -> Result<()> {
    if mode == PartsMode::Color {
        let mesh = extrude_groups(&groups, z0, z1)?;
        out.push(Part {
            name: format!("color_{:02}_{}", region.color_index + 1, region.color.hex()),
            color: region.color,
            mesh,
            is_backing: false,
            region_index: ri,
        });
    } else {
        for (i, g) in groups.into_iter().enumerate() {
            let mesh = extrude_groups(&[g], z0, z1)?;
            out.push(Part {
                name: format!(
                    "color_{:02}_{}_region_{:03}",
                    region.color_index + 1,
                    region.color.hex(),
                    i + 1
                ),
                color: region.color,
                mesh,
                is_backing: false,
                region_index: ri,
            });
        }
    }
    Ok(())
}

fn group_contours(contours: &[Contour]) -> Vec<(Contour, Vec<Contour>)> {
    let mut outers: Vec<(Contour, Vec<Contour>)> = Vec::new();
    let mut holes = Vec::new();
    for c in contours {
        if c.len() < 3 {
            continue;
        }
        if signed_area(c) < 0.0 {
            outers.push((c.clone(), Vec::new()));
        } else {
            holes.push(c.clone());
        }
    }
    // Pixel boundary extraction gives clockwise outer loops and counterclockwise
    // hole loops.  Assign holes by containment, then normalize winding for earcut.
    for h in holes {
        if let Some((_, hs)) = outers.iter_mut().find(|(o, _)| point_in_polygon(h[0], o)) {
            hs.push(h);
        }
    }
    for (o, hs) in outers.iter_mut() {
        if signed_area(o) < 0.0 {
            o.reverse();
        }
        for h in hs.iter_mut() {
            if signed_area(h) > 0.0 {
                h.reverse();
            }
        }
    }
    outers
}

fn extrude_groups(groups: &[(Contour, Vec<Contour>)], z0: f64, z1: f64) -> Result<Mesh> {
    if z1 <= z0 {
        return Err(anyhow!("extrusion depth must be positive"));
    }
    let mut mesh = Mesh::default();
    for (outer, holes) in groups {
        let mut loops = Vec::with_capacity(1 + holes.len());
        loops.push(outer.clone());
        loops.extend(holes.iter().cloned());
        let mut coords = Vec::new();
        let mut hole_indices = Vec::new();
        let mut count = 0;
        for (li, c) in loops.iter().enumerate() {
            if li > 0 {
                hole_indices.push(count);
            }
            count += c.len();
            for p in c {
                coords.push(p.x);
                coords.push(p.y);
            }
        }
        let indices = earcutr::earcut(&coords, &hole_indices, 2)
            .map_err(|e| anyhow!("triangulation failed: {e:?}"))?;
        let base = mesh.vertices.len() as u32;
        for c in &loops {
            for p in c {
                mesh.vertices.push(Vertex {
                    x: p.x,
                    y: p.y,
                    z: z0,
                });
            }
        }
        for c in &loops {
            for p in c {
                mesh.vertices.push(Vertex {
                    x: p.x,
                    y: p.y,
                    z: z1,
                });
            }
        }
        let total = count as u32;
        for tri in indices.chunks_exact(3) {
            let a = base + tri[0] as u32;
            let b = base + tri[1] as u32;
            let c = base + tri[2] as u32;
            let pa = mesh.vertices[a as usize];
            let pb = mesh.vertices[b as usize];
            let pc = mesh.vertices[c as usize];
            let cross = (pb.x - pa.x) * (pc.y - pa.y) - (pb.y - pa.y) * (pc.x - pa.x);
            if cross >= 0.0 {
                mesh.triangles.push(Triangle {
                    v1: a + total,
                    v2: b + total,
                    v3: c + total,
                });
                mesh.triangles.push(Triangle {
                    v1: c,
                    v2: b,
                    v3: a,
                });
            } else {
                mesh.triangles.push(Triangle {
                    v1: c + total,
                    v2: b + total,
                    v3: a + total,
                });
                mesh.triangles.push(Triangle {
                    v1: a,
                    v2: b,
                    v3: c,
                });
            }
        }
        let mut offset = 0u32;
        for loop_points in &loops {
            let n = loop_points.len();
            for i in 0..n {
                let j = (i + 1) % n;
                let a = base + offset + i as u32;
                let b = base + offset + j as u32;
                let at = a + total;
                let bt = b + total;
                mesh.triangles.push(Triangle {
                    v1: a,
                    v2: b,
                    v3: bt,
                });
                mesh.triangles.push(Triangle {
                    v1: a,
                    v2: bt,
                    v3: at,
                });
            }
            offset += n as u32;
        }
    }
    Ok(mesh)
}

pub fn bounds(parts: &[Part]) -> (f64, f64) {
    let mut maxx: f64 = 0.0;
    let mut maxy: f64 = 0.0;
    for p in parts {
        for v in &p.mesh.vertices {
            maxx = maxx.max(v.x);
            maxy = maxy.max(v.y);
        }
    }
    (maxx, maxy)
}

pub fn validate_parts(parts: &[Part]) -> Result<()> {
    for part in parts {
        if part.mesh.vertices.is_empty() || part.mesh.triangles.is_empty() {
            return Err(anyhow!("part {} has empty mesh", part.name));
        }
        for t in &part.mesh.triangles {
            if t.v1 as usize >= part.mesh.vertices.len()
                || t.v2 as usize >= part.mesh.vertices.len()
                || t.v3 as usize >= part.mesh.vertices.len()
            {
                return Err(anyhow!("part {} has invalid triangle index", part.name));
            }
            if t.v1 == t.v2 || t.v2 == t.v3 || t.v1 == t.v3 {
                return Err(anyhow!("part {} has a degenerate triangle", part.name));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn volume(mesh: &Mesh) -> f64 {
        mesh.triangles
            .iter()
            .map(|t| {
                let a = mesh.vertices[t.v1 as usize];
                let b = mesh.vertices[t.v2 as usize];
                let c = mesh.vertices[t.v3 as usize];
                (a.x * (b.y * c.z - b.z * c.y) - a.y * (b.x * c.z - b.z * c.x)
                    + a.z * (b.x * c.y - b.y * c.x))
                    / 6.0
            })
            .sum::<f64>()
            .abs()
    }
    #[test]
    fn cube_has_expected_volume() {
        let c = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        let m = extrude_groups(&[(c, Vec::new())], 0.0, 3.0).unwrap();
        assert!((volume(&m) - 300.0).abs() < 1e-6);
    }
    #[test]
    fn hole_has_expected_volume() {
        let o = vec![
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(10.0, 10.0),
            Point::new(0.0, 10.0),
        ];
        let h = vec![
            Point::new(3.0, 3.0),
            Point::new(3.0, 7.0),
            Point::new(7.0, 7.0),
            Point::new(7.0, 3.0),
        ];
        let m = extrude_groups(&[(o, vec![h])], 0.0, 3.0).unwrap();
        assert!((volume(&m) - 252.0).abs() < 1e-6);
    }
}
