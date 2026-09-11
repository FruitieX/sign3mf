use crate::color::Rgb;
use crate::geometry::{
    bounds as contour_bounds, point_in_polygon, signed_area, simplify_collinear, Contour, Point,
};
use crate::trace::{Region, Traced};
use crate::{Config, HolePosition, Mode, PartsMode};
use anyhow::{anyhow, Result};
use i_overlay::core::fill_rule::FillRule;
use i_overlay::core::overlay_rule::OverlayRule;
use i_overlay::float::single::SingleFloatOverlay;
use std::collections::HashMap;

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
    let model_width = source_w * scale;
    let model_height = source_h * scale;
    let raw_hole_specs = build_hole_specs(
        &config.holes,
        model_width,
        model_height,
        config.hole_diameter,
        config.hole_inset,
    )?;
    let orient_model = |p: Point| {
        let mut x = p.x;
        if config.upside_down {
            x = model_width - x;
        }
        Point::new(x, p.y)
    };
    let transform = |p: Point| {
        let mut x = (p.x - raw_bounds.0) * scale;
        let y = (p.y - raw_bounds.1) * scale;
        if config.upside_down {
            x = model_width - x;
        }
        Point::new(x, y)
    };
    let hole_specs: Vec<(Point, Contour)> = raw_hole_specs
        .into_iter()
        .map(|(center, path)| {
            (
                orient_model(center),
                path.into_iter().map(&orient_model).collect(),
            )
        })
        .collect();
    let hole_paths: Vec<Contour> = hole_specs.iter().map(|(_, path)| path.clone()).collect();
    let normalize_groups = |cs: &[Contour]| -> Vec<(Contour, Vec<Contour>)> {
        let transformed: Vec<Contour> = cs
            .iter()
            .map(|contour| contour.iter().copied().map(&transform).collect())
            .collect();
        subtract_holes(group_contours(&transformed), &hole_paths)
    };
    if !hole_specs.is_empty() {
        let transformed_backing: Vec<Contour> = traced
            .backing
            .iter()
            .map(|contour| contour.iter().copied().map(&transform).collect())
            .collect();
        let backing_groups = group_contours(&transformed_backing);
        validate_hole_containment(&backing_groups, &hole_specs)?;
    }
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
        let groups = normalize_groups(&traced.backing);
        let mesh = extrude_groups(&groups, 0.0, depth - config.face_depth)?;
        parts.push(Part {
            name: format!("backing_{}", backing_color.hex()),
            color: backing_color,
            mesh,
            is_backing: true,
            region_index: usize::MAX,
        });
        for (ri, region) in traced.regions.iter().enumerate() {
            let groups = normalize_groups(&region.contours);
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
            let groups = normalize_groups(&region.contours);
            add_region_parts(&mut parts, region, ri, groups, 0.0, depth, config.parts)?;
        }
    }
    if config.upside_down {
        for part in &mut parts {
            for vertex in &mut part.mesh.vertices {
                vertex.z = depth - vertex.z;
            }
            for triangle in &mut part.mesh.triangles {
                std::mem::swap(&mut triangle.v2, &mut triangle.v3);
            }
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

fn build_hole_specs(
    positions: &[HolePosition],
    width: f64,
    height: f64,
    diameter: f64,
    inset: f64,
) -> Result<Vec<(Point, Contour)>> {
    if positions.is_empty() {
        return Ok(Vec::new());
    }
    let radius = diameter / 2.0;
    let mut centers = Vec::new();
    let mut add_center = |center: Point| {
        if !centers.iter().any(|existing: &Point| {
            (existing.x - center.x).abs() <= 1e-9 && (existing.y - center.y).abs() <= 1e-9
        }) {
            centers.push(center);
        }
    };
    for position in positions {
        match position {
            HolePosition::Corners => {
                add_center(Point::new(inset, inset));
                add_center(Point::new(width - inset, inset));
                add_center(Point::new(width - inset, height - inset));
                add_center(Point::new(inset, height - inset));
            }
            HolePosition::CenterTop => add_center(Point::new(width / 2.0, height - inset)),
            HolePosition::CenterLeft => add_center(Point::new(inset, height / 2.0)),
            HolePosition::CenterRight => add_center(Point::new(width - inset, height / 2.0)),
            HolePosition::CenterBottom => add_center(Point::new(width / 2.0, inset)),
        }
    }
    for center in &centers {
        if center.x - radius <= 0.0
            || center.x + radius >= width
            || center.y - radius <= 0.0
            || center.y + radius >= height
        {
            return Err(anyhow!(
                "hole at ({:.2}, {:.2}) mm does not fit inside the {:.2} × {:.2} mm sign; increase --hole-inset or reduce --hole-diameter",
                center.x,
                center.y,
                width,
                height
            ));
        }
    }
    for (i, a) in centers.iter().enumerate() {
        for b in centers.iter().skip(i + 1) {
            if a.distance(*b) < diameter - 1e-9 {
                return Err(anyhow!(
                    "screw holes at ({:.2}, {:.2}) mm and ({:.2}, {:.2}) mm overlap",
                    a.x,
                    a.y,
                    b.x,
                    b.y
                ));
            }
        }
    }
    let specs = centers
        .into_iter()
        .map(|center| {
            let path = (0..64)
                .map(|i| {
                    let angle = std::f64::consts::TAU * i as f64 / 64.0;
                    Point::new(
                        center.x + radius * angle.cos(),
                        center.y + radius * angle.sin(),
                    )
                })
                .collect();
            (center, path)
        })
        .collect();
    Ok(specs)
}

fn validate_hole_containment(
    groups: &[(Contour, Vec<Contour>)],
    specs: &[(Point, Contour)],
) -> Result<()> {
    for (center, path) in specs {
        let contained = point_in_groups(*center, groups)
            && path
                .iter()
                .copied()
                .all(|point| point_in_groups(point, groups));
        if !contained {
            return Err(anyhow!(
                "hole at ({:.2}, {:.2}) mm is outside the printable artwork; choose another placement",
                center.x,
                center.y
            ));
        }
    }
    Ok(())
}

fn point_in_groups(point: Point, groups: &[(Contour, Vec<Contour>)]) -> bool {
    groups.iter().any(|(outer, holes)| {
        point_in_polygon(point, outer) && !holes.iter().any(|hole| point_in_polygon(point, hole))
    })
}

fn subtract_holes(
    groups: Vec<(Contour, Vec<Contour>)>,
    hole_paths: &[Contour],
) -> Vec<(Contour, Vec<Contour>)> {
    if hole_paths.is_empty() || groups.is_empty() {
        return groups;
    }
    let subject: Vec<Vec<[f64; 2]>> = groups
        .iter()
        .flat_map(|(outer, holes)| {
            std::iter::once(outer)
                .chain(holes.iter())
                .map(|contour| contour.iter().map(|p| [p.x, p.y]).collect())
        })
        .collect();
    let clips: Vec<Vec<[f64; 2]>> = hole_paths
        .iter()
        .map(|contour| contour.iter().map(|p| [p.x, p.y]).collect())
        .collect();
    let cleaned = subject.overlay(&clips, OverlayRule::Difference, FillRule::EvenOdd);
    groups_from_overlay(cleaned)
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
    if contours.is_empty() {
        return Vec::new();
    }

    // The pixel tracer can emit several loops meeting at a lattice vertex. A
    // direct containment pass cannot distinguish those touching loops from
    // genuine holes, and earcut then bridges the wrong boundaries with very
    // long triangles. Run the complete contour set through the polygon
    // overlay engine first. It resolves touching/self-intersecting boundaries
    // and returns one outer contour followed by its holes for each connected
    // filled component.
    let subject: Vec<Vec<[f64; 2]>> = contours
        .iter()
        .filter(|c| c.len() >= 3)
        .map(|c| c.iter().map(|p| [p.x, p.y]).collect())
        .collect();
    if subject.is_empty() {
        return Vec::new();
    }
    let empty_clip: Vec<[f64; 2]> = Vec::new();
    let cleaned = subject.overlay(&empty_clip, OverlayRule::Union, FillRule::EvenOdd);
    groups_from_overlay(cleaned)
}

fn groups_from_overlay(cleaned: Vec<Vec<Vec<[f64; 2]>>>) -> Vec<(Contour, Vec<Contour>)> {
    let mut groups = Vec::new();
    for shape in cleaned {
        let mut loops = shape.into_iter().filter(|c| c.len() >= 3);
        let Some(outer) = loops.next() else {
            continue;
        };
        let mut outer: Contour = outer.into_iter().map(|p| Point::new(p[0], p[1])).collect();
        simplify_collinear(&mut outer, 1e-9);
        let holes = loops
            .map(|hole| {
                let mut contour: Contour =
                    hole.into_iter().map(|p| Point::new(p[0], p[1])).collect();
                simplify_collinear(&mut contour, 1e-9);
                contour
            })
            .filter(|hole: &Contour| hole.len() >= 3)
            .collect();
        groups.push((outer, holes));
    }
    groups
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
        // Earcut may discard a lattice vertex while curing a local
        // intersection. Build the walls from the boundary edges that the cap
        // triangulation actually retained; using every source contour edge in
        // that case leaves an open shell and makes slicers show z-fighting.
        let raw_indices: Vec<[usize; 3]> = indices
            .chunks_exact(3)
            .map(|tri| [tri[0], tri[1], tri[2]])
            .collect();
        let mut cap_edges: HashMap<(usize, usize), (usize, (usize, usize))> = HashMap::new();
        for [a, b, c] in &raw_indices {
            for &(a, b) in &[(a, b), (b, c), (c, a)] {
                let key = (*a.min(b), *a.max(b));
                let entry = cap_edges.entry(key).or_insert((0, (*a, *b)));
                entry.0 += 1;
            }
        }
        let boundary_edges: Vec<(usize, usize)> = cap_edges
            .into_values()
            .filter_map(|(count, directed)| (count == 1).then_some(directed))
            .collect();
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
        for _ in 0..64 {
            let mut repaired = false;
            for tri in &raw_indices {
                let top = [
                    base + total + tri[0] as u32,
                    base + total + tri[1] as u32,
                    base + total + tri[2] as u32,
                ];
                let bottom = [
                    base + tri[2] as u32,
                    base + tri[1] as u32,
                    base + tri[0] as u32,
                ];
                let points = top.map(|index| mesh.vertices[index as usize]);
                if triangle_area2(points[0], points[1], points[2]).abs()
                    <= DEGENERATE_AREA2_THRESHOLD
                {
                    repair_degenerate_cap(&mut mesh, top, bottom);
                    repaired = true;
                }
            }
            if !repaired {
                break;
            }
        }
        for [a, b, c] in raw_indices {
            let a = base + a as u32;
            let b = base + b as u32;
            let c = base + c as u32;
            let top = [a + total, b + total, c + total];
            let bottom = [c, b, a];
            subdivide_cap_triangle(&mut mesh, top, bottom, 0);
        }
        for (a, b) in boundary_edges {
            let a = base + a as u32;
            let b = base + b as u32;
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
    }
    Ok(mesh)
}

const CAP_ASPECT_LIMIT: f64 = 20.0;
const CAP_SUBDIVISION_DEPTH: u8 = 4;
const COLLINEAR_REPAIR_EPSILON: f64 = 1e-6;
const DEGENERATE_AREA2_THRESHOLD: f64 = 1e-8;

fn repair_degenerate_cap(mesh: &mut Mesh, top: [u32; 3], bottom: [u32; 3]) {
    let points = top.map(|index| mesh.vertices[index as usize]);
    let mut middle = 0;
    let mut best = f64::INFINITY;
    let distance = |a: Vertex, b: Vertex| ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
    for i in 0..3 {
        let j = (i + 1) % 3;
        let k = (i + 2) % 3;
        let score = (distance(points[i], points[j]) + distance(points[i], points[k])
            - distance(points[j], points[k]))
        .abs();
        if score < best {
            best = score;
            middle = i;
        }
    }
    let j = (middle + 1) % 3;
    let k = (middle + 2) % 3;
    let dx = points[k].x - points[j].x;
    let dy = points[k].y - points[j].y;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= 1e-12 {
        return;
    }
    let nx = -dy / length * COLLINEAR_REPAIR_EPSILON;
    let ny = dx / length * COLLINEAR_REPAIR_EPSILON;
    let corresponding_bottom = bottom[2 - middle];
    for vertex in [top[middle], corresponding_bottom] {
        mesh.vertices[vertex as usize].x += nx;
        mesh.vertices[vertex as usize].y += ny;
    }
}

fn subdivide_cap_triangle(mesh: &mut Mesh, top: [u32; 3], bottom: [u32; 3], depth: u8) {
    let pa = mesh.vertices[top[0] as usize];
    let pb = mesh.vertices[top[1] as usize];
    let pc = mesh.vertices[top[2] as usize];
    let aspect = triangle_aspect(pa, pb, pc);
    if aspect > CAP_ASPECT_LIMIT && depth < CAP_SUBDIVISION_DEPTH {
        let center = Vertex {
            x: (pa.x + pb.x + pc.x) / 3.0,
            y: (pa.y + pb.y + pc.y) / 3.0,
            z: pa.z,
        };
        let center_bottom = mesh.vertices.len() as u32;
        mesh.vertices.push(Vertex {
            z: mesh.vertices[bottom[0] as usize].z,
            ..center
        });
        let center_top = mesh.vertices.len() as u32;
        mesh.vertices.push(Vertex { z: pa.z, ..center });
        let can_subdivide = [
            [top[0], top[1], center_top],
            [top[1], top[2], center_top],
            [top[2], top[0], center_top],
        ]
        .into_iter()
        .all(|triangle| {
            let a = mesh.vertices[triangle[0] as usize];
            let b = mesh.vertices[triangle[1] as usize];
            let c = mesh.vertices[triangle[2] as usize];
            triangle_area2(a, b, c).abs() > DEGENERATE_AREA2_THRESHOLD
        });
        if !can_subdivide {
            mesh.vertices.truncate(center_bottom as usize);
            mesh.triangles.push(Triangle {
                v1: top[0],
                v2: top[1],
                v3: top[2],
            });
            mesh.triangles.push(Triangle {
                v1: bottom[0],
                v2: bottom[1],
                v3: bottom[2],
            });
            return;
        }
        subdivide_cap_triangle(
            mesh,
            [top[0], top[1], center_top],
            [center_bottom, bottom[1], bottom[2]],
            depth + 1,
        );
        subdivide_cap_triangle(
            mesh,
            [top[1], top[2], center_top],
            [center_bottom, bottom[0], bottom[1]],
            depth + 1,
        );
        subdivide_cap_triangle(
            mesh,
            [top[2], top[0], center_top],
            [center_bottom, bottom[2], bottom[0]],
            depth + 1,
        );
    } else {
        mesh.triangles.push(Triangle {
            v1: top[0],
            v2: top[1],
            v3: top[2],
        });
        mesh.triangles.push(Triangle {
            v1: bottom[0],
            v2: bottom[1],
            v3: bottom[2],
        });
    }
}

fn triangle_aspect(a: Vertex, b: Vertex, c: Vertex) -> f64 {
    let ab = ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
    let bc = ((b.x - c.x).powi(2) + (b.y - c.y).powi(2)).sqrt();
    let ca = ((c.x - a.x).powi(2) + (c.y - a.y).powi(2)).sqrt();
    let shortest = ab.min(bc).min(ca);
    if shortest <= 1e-12 {
        f64::INFINITY
    } else {
        ab.max(bc).max(ca) / shortest
    }
}

fn triangle_area2(a: Vertex, b: Vertex, c: Vertex) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
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
        let mut edges: HashMap<(u32, u32), usize> = HashMap::new();
        let mut edge_triangles: HashMap<(u32, u32), Vec<usize>> = HashMap::new();
        let mut signed_volume = 0.0;
        for (ti, t) in part.mesh.triangles.iter().enumerate() {
            if t.v1 as usize >= part.mesh.vertices.len()
                || t.v2 as usize >= part.mesh.vertices.len()
                || t.v3 as usize >= part.mesh.vertices.len()
            {
                return Err(anyhow!("part {} has invalid triangle index", part.name));
            }
            if t.v1 == t.v2 || t.v2 == t.v3 || t.v1 == t.v3 {
                return Err(anyhow!("part {} has a degenerate triangle", part.name));
            }
            let a = part.mesh.vertices[t.v1 as usize];
            let b = part.mesh.vertices[t.v2 as usize];
            let c = part.mesh.vertices[t.v3 as usize];
            let cross = (
                (b.y - a.y) * (c.z - a.z) - (b.z - a.z) * (c.y - a.y),
                (b.z - a.z) * (c.x - a.x) - (b.x - a.x) * (c.z - a.z),
                (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x),
            );
            if cross.0 * cross.0 + cross.1 * cross.1 + cross.2 * cross.2 <= 1e-20 {
                return Err(anyhow!(
                    "part {} has a zero-area triangle ({}, {}, {})",
                    part.name,
                    t.v1,
                    t.v2,
                    t.v3
                ));
            }
            for (u, v) in [(t.v1, t.v2), (t.v2, t.v3), (t.v3, t.v1)] {
                *edges.entry((u.min(v), u.max(v))).or_default() += 1;
                edge_triangles
                    .entry((u.min(v), u.max(v)))
                    .or_default()
                    .push(ti);
            }
            signed_volume += (a.x * (b.y * c.z - b.z * c.y) - a.y * (b.x * c.z - b.z * c.x)
                + a.z * (b.x * c.y - b.y * c.x))
                / 6.0;
        }
        if edges.values().any(|count| *count != 2) {
            let bad = edges.values().filter(|count| **count != 2).count();
            let examples = edges
                .iter()
                .filter(|(_, count)| **count != 2)
                .take(4)
                .map(|((a, b), count)| format!("{}-{}:{}", a, b, count))
                .collect::<Vec<_>>()
                .join(",");
            let edge_detail = edges
                .iter()
                .filter(|(_, count)| **count != 2)
                .take(1)
                .map(|(edge, _)| format!("{:?}", edge_triangles.get(edge).unwrap()))
                .next()
                .unwrap_or_default();
            return Err(anyhow!(
                "part {} is not a closed triangle shell ({} edges: {}; triangles {})",
                part.name,
                bad,
                examples,
                edge_detail
            ));
        }
        if signed_volume.abs() <= 1e-9 {
            return Err(anyhow!("part {} has zero volume", part.name));
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

    #[test]
    fn screw_hole_subtraction_creates_a_closed_cutout() {
        let outer = vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
            Point::new(0.0, 100.0),
        ];
        let (_, holes) = build_hole_specs(&[HolePosition::CenterTop], 100.0, 100.0, 10.0, 15.0)
            .unwrap()
            .remove(0);
        let groups = subtract_holes(group_contours(&[outer]), &[holes]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].1.len(), 1);
        let mesh = extrude_groups(&groups, 0.0, 3.0).unwrap();
        validate_parts(&[Part {
            name: "test".into(),
            color: Rgb(0, 0, 0),
            mesh: mesh.clone(),
            is_backing: true,
            region_index: 0,
        }])
        .unwrap();
        let removed_area = signed_area(&groups[0].1[0]).abs();
        assert!((volume(&mesh) - (100.0 * 100.0 - removed_area) * 3.0).abs() < 1e-5);
    }
}
