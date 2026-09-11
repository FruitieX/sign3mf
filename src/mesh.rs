use crate::color::Rgb;
use crate::geometry::{
    bounds as contour_bounds, point_in_polygon, signed_area, simplify_collinear, Contour, Point,
};
use crate::trace::{Region, Traced};
use crate::{Config, CountersinkSide, HolePosition, HoleStyle, Mode, PartsMode};
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

#[derive(Debug, Clone, Copy)]
struct CountersinkHole {
    center: Point,
    bore_radius: f64,
    opening_radius: f64,
    depth: f64,
}

#[derive(Debug, Clone, Copy)]
enum OuterContourKind {
    Original,
    Chamfered,
    Inset,
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
    let countersink_holes = if config.hole_style == HoleStyle::Countersunk {
        let angle = config.countersink_angle.to_radians();
        let depth = config
            .countersink_diameter
            .map_or(config.countersink_depth, |diameter| {
                (diameter - config.hole_diameter) / 2.0 / (angle / 2.0).tan()
            });
        let opening_radius = config.countersink_diameter.map_or(
            config.hole_diameter / 2.0 + depth * (angle / 2.0).tan(),
            |diameter| diameter / 2.0,
        );
        Some(
            hole_specs
                .iter()
                .map(|(center, _)| CountersinkHole {
                    center: *center,
                    bore_radius: config.hole_diameter / 2.0,
                    opening_radius,
                    depth,
                })
                .collect::<Vec<_>>(),
        )
    } else {
        None
    };
    let transformed_backing: Vec<Contour> = traced
        .backing
        .iter()
        .map(|contour| contour.iter().copied().map(&transform).collect())
        .collect();
    let mut backing_groups = group_contours(&transformed_backing);
    if config.corner_radius > 0.0 {
        backing_groups = round_groups(&backing_groups, config.corner_radius)?;
    }
    let chamfer_boundaries: Vec<Contour> = backing_groups
        .iter()
        .map(|(outer, _)| outer.clone())
        .collect();
    let clip_groups = (config.corner_radius > 0.0).then(|| backing_groups.clone());
    let normalize_groups = |cs: &[Contour]| -> Vec<(Contour, Vec<Contour>)> {
        let transformed: Vec<Contour> = cs
            .iter()
            .map(|contour| contour.iter().copied().map(&transform).collect())
            .collect();
        let groups = group_contours(&transformed);
        let groups = if let Some(clip) = clip_groups.as_ref() {
            intersect_groups(groups, clip)
        } else {
            groups
        };
        subtract_holes(groups, &hole_paths)
    };
    if !hole_specs.is_empty() {
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
        let groups = subtract_holes(backing_groups.clone(), &hole_paths);
        let mesh = extrude_for_config(
            &groups,
            0.0,
            depth - config.face_depth,
            config.chamfer,
            &chamfer_boundaries,
            true,
            false,
            countersink_holes.as_deref(),
            config.countersink_side == CountersinkSide::Back,
            false,
        )?;
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
                config.chamfer,
                &chamfer_boundaries,
                false,
                true,
                countersink_holes.as_deref(),
                config.countersink_side == CountersinkSide::Front,
                true,
            )?;
        }
    } else {
        for (ri, region) in traced.regions.iter().enumerate() {
            let groups = normalize_groups(&region.contours);
            add_region_parts(
                &mut parts,
                region,
                ri,
                groups,
                0.0,
                depth,
                config.parts,
                config.chamfer,
                &chamfer_boundaries,
                true,
                true,
                countersink_holes.as_deref(),
                true,
                config.countersink_side == CountersinkSide::Front,
            )?;
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

fn intersect_groups(
    subject_groups: Vec<(Contour, Vec<Contour>)>,
    clip_groups: &[(Contour, Vec<Contour>)],
) -> Vec<(Contour, Vec<Contour>)> {
    if subject_groups.is_empty() || clip_groups.is_empty() {
        return Vec::new();
    }
    let subject = flatten_groups(&subject_groups);
    let clips = flatten_groups(clip_groups);
    let cleaned = subject.overlay(&clips, OverlayRule::Intersect, FillRule::EvenOdd);
    groups_from_overlay(cleaned)
}

fn flatten_groups(groups: &[(Contour, Vec<Contour>)]) -> Vec<Vec<[f64; 2]>> {
    groups
        .iter()
        .flat_map(|(outer, holes)| {
            std::iter::once(outer)
                .chain(holes.iter())
                .map(|contour| contour.iter().map(|p| [p.x, p.y]).collect())
        })
        .collect()
}

fn round_groups(
    groups: &[(Contour, Vec<Contour>)],
    radius: f64,
) -> Result<Vec<(Contour, Vec<Contour>)>> {
    groups
        .iter()
        .map(|(outer, holes)| Ok((fillet_contour(outer, radius)?, holes.clone())))
        .collect()
}

fn fillet_contour(contour: &Contour, radius: f64) -> Result<Contour> {
    if contour.len() < 3 || radius <= 0.0 {
        return Ok(contour.clone());
    }
    let orientation = signed_area(contour).signum();
    if orientation == 0.0 {
        return Ok(contour.clone());
    }
    let mut result = Vec::new();
    for i in 0..contour.len() {
        let prev = contour[(i + contour.len() - 1) % contour.len()];
        let current = contour[i];
        let next = contour[(i + 1) % contour.len()];
        let incoming = Point::new(prev.x - current.x, prev.y - current.y);
        let outgoing = Point::new(next.x - current.x, next.y - current.y);
        let prev_len = current.distance(prev);
        let next_len = current.distance(next);
        if prev_len <= 1e-9 || next_len <= 1e-9 {
            result.push(current);
            continue;
        }
        let turn = (current.x - prev.x) * (next.y - current.y)
            - (current.y - prev.y) * (next.x - current.x);
        if turn * orientation <= 1e-9 {
            result.push(current);
            continue;
        }
        let a = Point::new(incoming.x / prev_len, incoming.y / prev_len);
        let b = Point::new(outgoing.x / next_len, outgoing.y / next_len);
        let dot = (a.x * b.x + a.y * b.y).clamp(-1.0, 1.0);
        let angle = dot.acos();
        if angle <= 1e-6 || angle >= 170.0_f64.to_radians() {
            result.push(current);
            continue;
        }
        let tangent_distance = radius / (angle / 2.0).tan();
        if tangent_distance * 2.0 >= prev_len.min(next_len) {
            return Err(anyhow!(
                "--corner-radius {:.3} mm is too large for a sign corner; reduce it below {:.3} mm",
                radius,
                tangent_distance.min(prev_len.min(next_len) / 2.0)
            ));
        }
        let bisector = Point::new(a.x + b.x, a.y + b.y);
        let bisector_len = (bisector.x * bisector.x + bisector.y * bisector.y).sqrt();
        if bisector_len <= 1e-9 {
            result.push(current);
            continue;
        }
        let center_distance = radius / (angle / 2.0).sin();
        let center = Point::new(
            current.x + bisector.x / bisector_len * center_distance,
            current.y + bisector.y / bisector_len * center_distance,
        );
        let start = Point::new(
            current.x + a.x * tangent_distance,
            current.y + a.y * tangent_distance,
        );
        let sweep = std::f64::consts::PI - angle;
        let start_angle = (start.y - center.y).atan2(start.x - center.x);
        let segments = ((radius * sweep / 0.25).ceil() as usize).clamp(3, 24);
        result.push(start);
        for step in 1..=segments {
            let theta = start_angle + orientation * sweep * step as f64 / segments as f64;
            result.push(Point::new(
                center.x + radius * theta.cos(),
                center.y + radius * theta.sin(),
            ));
        }
    }
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
fn add_region_parts(
    out: &mut Vec<Part>,
    region: &Region,
    ri: usize,
    groups: Vec<(Contour, Vec<Contour>)>,
    z0: f64,
    z1: f64,
    mode: PartsMode,
    chamfer: f64,
    chamfer_boundaries: &[Contour],
    chamfer_bottom: bool,
    chamfer_top: bool,
    countersink_holes: Option<&[CountersinkHole]>,
    apply_countersink: bool,
    countersink_from_top: bool,
) -> Result<()> {
    if mode == PartsMode::Color {
        let mesh = extrude_for_config(
            &groups,
            z0,
            z1,
            chamfer,
            chamfer_boundaries,
            chamfer_bottom,
            chamfer_top,
            countersink_holes,
            apply_countersink,
            countersink_from_top,
        )?;
        out.push(Part {
            name: format!("color_{:02}_{}", region.color_index + 1, region.color.hex()),
            color: region.color,
            mesh,
            is_backing: false,
            region_index: ri,
        });
    } else {
        for (i, g) in groups.into_iter().enumerate() {
            let mesh = extrude_for_config(
                &[g],
                z0,
                z1,
                chamfer,
                chamfer_boundaries,
                chamfer_bottom,
                chamfer_top,
                countersink_holes,
                apply_countersink,
                countersink_from_top,
            )?;
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

#[allow(clippy::too_many_arguments)]
fn extrude_for_config(
    groups: &[(Contour, Vec<Contour>)],
    z0: f64,
    z1: f64,
    chamfer: f64,
    chamfer_boundaries: &[Contour],
    chamfer_bottom: bool,
    chamfer_top: bool,
    countersink_holes: Option<&[CountersinkHole]>,
    apply_countersink: bool,
    countersink_from_top: bool,
) -> Result<Mesh> {
    let countersink_holes = if apply_countersink {
        countersink_holes.unwrap_or(&[])
    } else {
        &[]
    };
    let mut mesh = Mesh::default();
    for group in groups {
        let use_countersink = !countersink_holes.is_empty();
        let use_chamfer = chamfer > 0.0
            && (chamfer_bottom || chamfer_top)
            && chamfer_boundaries
                .iter()
                .any(|boundary| contours_match(&group.0, boundary));
        let piece = if use_countersink && use_chamfer {
            extrude_groups_countersunk_chamfered(
                std::slice::from_ref(group),
                z0,
                z1,
                countersink_holes,
                countersink_from_top,
                chamfer,
                chamfer_bottom,
                chamfer_top,
            )?
        } else if use_countersink {
            extrude_groups_countersunk(
                std::slice::from_ref(group),
                z0,
                z1,
                countersink_holes,
                countersink_from_top,
            )?
        } else if use_chamfer {
            extrude_groups_chamfered(
                std::slice::from_ref(group),
                z0,
                z1,
                chamfer,
                chamfer_bottom,
                chamfer_top,
            )?
        } else {
            extrude_groups(std::slice::from_ref(group), z0, z1)?
        };
        append_mesh(&mut mesh, piece);
    }
    Ok(mesh)
}

fn contours_match(a: &Contour, b: &Contour) -> bool {
    let Some(a_bounds) = contour_bounds(std::slice::from_ref(a)) else {
        return false;
    };
    let Some(b_bounds) = contour_bounds(std::slice::from_ref(b)) else {
        return false;
    };
    let scale = a_bounds
        .2
        .abs()
        .max(a_bounds.3.abs())
        .max(b_bounds.2.abs())
        .max(b_bounds.3.abs())
        .max(1.0);
    let bounds_match = (0..4).all(|i| {
        let a_value = [a_bounds.0, a_bounds.1, a_bounds.2, a_bounds.3][i];
        let b_value = [b_bounds.0, b_bounds.1, b_bounds.2, b_bounds.3][i];
        (a_value - b_value).abs() <= scale * 1e-7
    });
    bounds_match && (signed_area(a).abs() - signed_area(b).abs()).abs() <= scale * scale * 1e-7
}

fn append_mesh(destination: &mut Mesh, source: Mesh) {
    let base = destination.vertices.len() as u32;
    destination.vertices.extend(source.vertices);
    destination
        .triangles
        .extend(source.triangles.into_iter().map(|t| Triangle {
            v1: t.v1 + base,
            v2: t.v2 + base,
            v3: t.v3 + base,
        }));
}

fn extrude_groups_countersunk(
    groups: &[(Contour, Vec<Contour>)],
    z0: f64,
    z1: f64,
    countersink_holes: &[CountersinkHole],
    from_top: bool,
) -> Result<Mesh> {
    if countersink_holes.is_empty() {
        return extrude_groups(groups, z0, z1);
    }
    let depth = countersink_holes[0].depth;
    if z1 <= z0 || depth <= 0.0 || depth > z1 - z0 + 1e-9 {
        return Err(anyhow!("countersink depth does not fit the part extrusion"));
    }
    let mut mesh = Mesh::default();
    for (outer, holes) in groups {
        let mut bore_loops = Vec::with_capacity(1 + holes.len());
        bore_loops.push(outer.clone());
        bore_loops.extend(holes.iter().cloned());
        let opening_loops = scale_countersink_holes(&bore_loops, countersink_holes);
        let mut layers = Vec::with_capacity(3);
        if from_top {
            layers.push((bore_loops.clone(), z0));
            if depth < z1 - z0 - 1e-9 {
                layers.push((bore_loops.clone(), z1 - depth));
            }
            layers.push((opening_loops, z1));
        } else {
            layers.push((opening_loops, z0));
            if depth < z1 - z0 - 1e-9 {
                layers.push((bore_loops.clone(), z0 + depth));
            }
            layers.push((bore_loops, z1));
        }
        let offsets: Vec<Vec<u32>> = layers
            .iter()
            .map(|(loops, z)| push_layer(&mut mesh, loops, *z))
            .collect();
        let cap_indices = triangulate_loops(&layers[0].0)?;
        let cap_boundary_edges = boundary_edges_from_indices(&cap_indices);
        if !boundary_edges_form_loops(&cap_boundary_edges, &layers[0].0) {
            return extrude_groups(groups, z0, z1);
        }
        repair_cap_triangles(
            &mut mesh,
            &offsets[0],
            offsets.last().unwrap(),
            &cap_indices,
        );
        add_cap_triangles(&mut mesh, &offsets[0], &cap_indices, false);
        add_cap_triangles(&mut mesh, offsets.last().unwrap(), &cap_indices, true);
        for pair in offsets.windows(2) {
            add_loop_rings(&mut mesh, &pair[0], &pair[1], &cap_boundary_edges);
        }
    }
    Ok(mesh)
}

#[allow(clippy::too_many_arguments)]
fn extrude_groups_countersunk_chamfered(
    groups: &[(Contour, Vec<Contour>)],
    z0: f64,
    z1: f64,
    countersink_holes: &[CountersinkHole],
    from_top: bool,
    chamfer: f64,
    chamfer_bottom: bool,
    chamfer_top: bool,
) -> Result<Mesh> {
    if countersink_holes.is_empty() {
        return extrude_groups_chamfered(groups, z0, z1, chamfer, chamfer_bottom, chamfer_top);
    }
    if z1 <= z0 {
        return Err(anyhow!("extrusion depth must be positive"));
    }
    let depth = z1 - z0;
    let sink_depth = countersink_holes[0].depth;
    if !sink_depth.is_finite() || sink_depth <= 0.0 {
        return Err(anyhow!("countersink depth must be positive"));
    }
    const MIN_BEVEL_SPAN: f64 = 1e-6;
    if (chamfer_bottom || chamfer_top) && depth <= MIN_BEVEL_SPAN {
        return Err(anyhow!(
            "chamfered extrusion depth must be greater than {MIN_BEVEL_SPAN} mm"
        ));
    }
    let available_cut = depth - MIN_BEVEL_SPAN;
    let bottom_cut = if chamfer_bottom {
        chamfer.min(available_cut)
    } else {
        0.0
    };
    let top_cut = if chamfer_top {
        chamfer.min(available_cut)
    } else {
        0.0
    };
    if (chamfer_bottom && bottom_cut <= 0.0)
        || (chamfer_top && top_cut <= 0.0)
        || (chamfer_bottom && chamfer_top && bottom_cut + top_cut >= depth)
    {
        return Err(anyhow!(
            "chamfer does not fit inside the part's extrusion depth"
        ));
    }

    let mut mesh = Mesh::default();
    for (outer, holes) in groups {
        let chamfered_outer = chamfer_contour(outer, chamfer);
        let inset_outer =
            offset_contour(&chamfered_outer, chamfer).unwrap_or_else(|_| chamfered_outer.clone());
        let mut resampled = common_resampled_contours(&[outer, &chamfered_outer, &inset_outer]);
        for contour in &mut resampled {
            perturb_collinear_points(contour);
        }
        let outer_variants = [&resampled[0], &resampled[1], &resampled[2]];
        let opening_holes = scale_countersink_hole_contours(holes, countersink_holes);
        let bottom_kind = if chamfer_bottom {
            OuterContourKind::Inset
        } else {
            OuterContourKind::Original
        };
        let top_kind = if chamfer_top {
            OuterContourKind::Inset
        } else {
            OuterContourKind::Original
        };
        let bottom_opening = !from_top;
        let top_opening = from_top;
        let make_loops = |kind: OuterContourKind, opening: bool| {
            let outer = match kind {
                OuterContourKind::Original => outer_variants[0],
                OuterContourKind::Chamfered => outer_variants[1],
                OuterContourKind::Inset => outer_variants[2],
            };
            let mut loops = Vec::with_capacity(1 + holes.len());
            loops.push(outer.clone());
            loops.extend(if opening {
                opening_holes.clone()
            } else {
                holes.clone()
            });
            loops
        };

        let mut transitions: Vec<(f64, Option<OuterContourKind>, Option<bool>)> =
            Vec::with_capacity(4);
        if chamfer_bottom {
            transitions.push((z0 + bottom_cut, Some(OuterContourKind::Chamfered), None));
        }
        if chamfer_top {
            transitions.push((z1 - top_cut, Some(OuterContourKind::Chamfered), None));
        }
        if sink_depth < depth - 1e-9 {
            if from_top {
                transitions.push((z1 - sink_depth, None, Some(false)));
            } else {
                transitions.push((z0 + sink_depth, None, Some(false)));
            }
        }
        transitions.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));

        let mut layers: Vec<(Vec<Contour>, f64)> = Vec::with_capacity(6);
        layers.push((make_loops(bottom_kind, bottom_opening), z0));
        let mut current_z = z0;
        let mut current_kind = bottom_kind;
        let mut current_opening = bottom_opening;
        for (event_z, kind, opening) in transitions {
            let event_z = event_z.clamp(z0, z1);
            if event_z > current_z + 1e-8 {
                layers.push((make_loops(current_kind, current_opening), event_z));
                current_z = event_z;
            }
            if let Some(kind) = kind {
                current_kind = kind;
            }
            if let Some(opening) = opening {
                current_opening = opening;
            }
        }
        layers.push((make_loops(top_kind, top_opening), z1));

        let offsets: Vec<Vec<u32>> = layers
            .iter()
            .map(|(loops, z)| push_layer(&mut mesh, loops, *z))
            .collect();
        let cap_indices = triangulate_loops(&layers[0].0)?;
        let cap_boundary_edges = boundary_edges_from_indices(&cap_indices);
        if !boundary_edges_form_loops(&cap_boundary_edges, &layers[0].0) {
            return extrude_groups_countersunk(groups, z0, z1, countersink_holes, from_top);
        }
        repair_degenerate_cap_across_layers(&mut mesh, &offsets, &cap_indices);
        add_cap_triangles(&mut mesh, &offsets[0], &cap_indices, false);
        add_cap_triangles(&mut mesh, offsets.last().unwrap(), &cap_indices, true);
        for pair in offsets.windows(2) {
            add_loop_rings(&mut mesh, &pair[0], &pair[1], &cap_boundary_edges);
        }
    }
    Ok(mesh)
}

fn scale_countersink_hole_contours(
    holes: &[Contour],
    countersink_holes: &[CountersinkHole],
) -> Vec<Contour> {
    holes
        .iter()
        .map(|contour| {
            let Some(hole) = countersink_holes
                .iter()
                .find(|hole| contour_matches_countersink(contour, hole))
            else {
                return contour.clone();
            };
            let factor = hole.opening_radius / hole.bore_radius;
            contour
                .iter()
                .map(|point| {
                    Point::new(
                        hole.center.x + (point.x - hole.center.x) * factor,
                        hole.center.y + (point.y - hole.center.y) * factor,
                    )
                })
                .collect()
        })
        .collect()
}

fn repair_cap_triangles(mesh: &mut Mesh, bottom: &[u32], top: &[u32], indices: &[usize]) {
    for tri in indices.chunks_exact(3) {
        let top_triangle = [
            vertex_index(top, tri[0]),
            vertex_index(top, tri[1]),
            vertex_index(top, tri[2]),
        ];
        let bottom_triangle = [
            vertex_index(bottom, tri[2]),
            vertex_index(bottom, tri[1]),
            vertex_index(bottom, tri[0]),
        ];
        let top_points = top_triangle.map(|index| mesh.vertices[index as usize]);
        if triangle_area2(top_points[0], top_points[1], top_points[2]).abs()
            <= DEGENERATE_AREA2_THRESHOLD
        {
            repair_degenerate_cap(mesh, top_triangle, bottom_triangle);
        }
        let bottom_points = bottom_triangle.map(|index| mesh.vertices[index as usize]);
        if triangle_area2(bottom_points[0], bottom_points[1], bottom_points[2]).abs()
            <= DEGENERATE_AREA2_THRESHOLD
        {
            repair_degenerate_cap(mesh, top_triangle, bottom_triangle);
        }
    }
}

fn scale_countersink_holes(
    loops: &[Contour],
    countersink_holes: &[CountersinkHole],
) -> Vec<Contour> {
    loops
        .iter()
        .enumerate()
        .map(|(index, contour)| {
            if index == 0 {
                return contour.clone();
            }
            let Some(hole) = countersink_holes
                .iter()
                .find(|hole| contour_matches_countersink(contour, hole))
            else {
                return contour.clone();
            };
            let factor = hole.opening_radius / hole.bore_radius;
            contour
                .iter()
                .map(|point| {
                    Point::new(
                        hole.center.x + (point.x - hole.center.x) * factor,
                        hole.center.y + (point.y - hole.center.y) * factor,
                    )
                })
                .collect()
        })
        .collect()
}

fn contour_matches_countersink(contour: &Contour, hole: &CountersinkHole) -> bool {
    if contour.is_empty() {
        return false;
    }
    let center = Point::new(
        contour.iter().map(|point| point.x).sum::<f64>() / contour.len() as f64,
        contour.iter().map(|point| point.y).sum::<f64>() / contour.len() as f64,
    );
    center.distance(hole.center) <= hole.bore_radius * 1.5 + 1e-5
}

fn extrude_groups_chamfered(
    groups: &[(Contour, Vec<Contour>)],
    z0: f64,
    z1: f64,
    chamfer: f64,
    chamfer_bottom: bool,
    chamfer_top: bool,
) -> Result<Mesh> {
    if z1 <= z0 {
        return Err(anyhow!("extrusion depth must be positive"));
    }
    if !chamfer_bottom && !chamfer_top {
        return extrude_groups(groups, z0, z1);
    }
    if !chamfer.is_finite() || chamfer <= 0.0 {
        return extrude_groups(groups, z0, z1);
    }

    if chamfer_bottom && chamfer_top {
        return extrude_groups_chamfered_symmetric(groups, z0, z1, chamfer);
    }

    let depth = z1 - z0;
    const MIN_BEVEL_SPAN: f64 = 1e-6;
    if depth <= MIN_BEVEL_SPAN {
        return Err(anyhow!(
            "chamfered extrusion depth must be greater than {MIN_BEVEL_SPAN} mm"
        ));
    }
    let available_cut = depth - MIN_BEVEL_SPAN;
    let bottom_cut = if chamfer_bottom {
        // In backed mode a face can be thinner than the requested horizontal
        // chamfer distance. Keep the interface planar and limit only the
        // vertical part of the bevel to the available layer depth.
        chamfer.min(available_cut)
    } else {
        0.0
    };
    let top_cut = if chamfer_top {
        chamfer.min(available_cut)
    } else {
        0.0
    };
    if (chamfer_bottom && bottom_cut <= 0.0)
        || (chamfer_top && top_cut <= 0.0)
        || (chamfer_bottom && chamfer_top && bottom_cut + top_cut >= depth)
    {
        return Err(anyhow!(
            "chamfer does not fit inside the part's extrusion depth"
        ));
    }

    let mut mesh = Mesh::default();
    for (outer, holes) in groups {
        let chamfered_outer = chamfer_contour(outer, chamfer);
        let inset_outer =
            offset_contour(&chamfered_outer, chamfer).unwrap_or_else(|_| chamfered_outer.clone());

        // Chamfering inserts points at corners. Resample the original and
        // chamfered contours onto the union of their perimeter fractions so
        // that an unchamfered interface can still share the same ring indices
        // without changing its outline.
        let mut resampled = common_resampled_contours(&[outer, &chamfered_outer, &inset_outer]);
        for contour in &mut resampled {
            perturb_collinear_points(contour);
        }
        let original_outer = &resampled[0];
        let chamfered_outer = &resampled[1];
        let inset_outer = &resampled[2];

        let bottom_cap_outer = if chamfer_bottom {
            inset_outer
        } else {
            original_outer
        };
        let bottom_transition_outer = if chamfer_bottom {
            chamfered_outer
        } else {
            original_outer
        };
        let top_transition_outer = if chamfer_top {
            chamfered_outer
        } else {
            original_outer
        };
        let top_cap_outer = if chamfer_top {
            inset_outer
        } else {
            original_outer
        };

        let make_loops = |outer: &Contour| {
            let mut loops = Vec::with_capacity(1 + holes.len());
            loops.push(outer.clone());
            loops.extend(holes.iter().cloned());
            loops
        };
        let bottom_cap_loops = make_loops(bottom_cap_outer);
        let bottom_transition_loops = make_loops(bottom_transition_outer);
        let top_transition_loops = make_loops(top_transition_outer);
        let top_cap_loops = make_loops(top_cap_outer);

        let mut layers: Vec<(Vec<Contour>, f64)> = Vec::with_capacity(4);
        layers.push((bottom_cap_loops.clone(), z0));
        if chamfer_bottom {
            layers.push((bottom_transition_loops, z0 + bottom_cut));
        }
        if chamfer_top {
            layers.push((top_transition_loops, z1 - top_cut));
        }
        layers.push((top_cap_loops, z1));

        let offsets: Vec<Vec<u32>> = layers
            .iter()
            .map(|(loops, z)| push_layer(&mut mesh, loops, *z))
            .collect();
        let cap_indices = triangulate_loops(&layers[0].0)?;
        let cap_boundary_edges = boundary_edges_from_indices(&cap_indices);
        if cap_boundary_edges.is_empty() {
            return extrude_groups(groups, z0, z1);
        }
        repair_degenerate_cap_across_layers(&mut mesh, &offsets, &cap_indices);
        add_cap_triangles(&mut mesh, &offsets[0], &cap_indices, false);
        add_cap_triangles(&mut mesh, offsets.last().unwrap(), &cap_indices, true);
        for pair in offsets.windows(2) {
            add_loop_rings(&mut mesh, &pair[0], &pair[1], &cap_boundary_edges);
        }
    }
    Ok(mesh)
}

fn extrude_groups_chamfered_symmetric(
    groups: &[(Contour, Vec<Contour>)],
    z0: f64,
    z1: f64,
    chamfer: f64,
) -> Result<Mesh> {
    if chamfer * 2.0 >= z1 - z0 {
        return Err(anyhow!(
            "chamfer distance must be smaller than half of the part's extrusion depth"
        ));
    }
    let mut mesh = Mesh::default();
    for (outer, holes) in groups {
        let mut loops = Vec::with_capacity(1 + holes.len());
        loops.push(chamfer_contour(outer, chamfer));
        loops.extend(holes.iter().cloned());
        let mut inner_loops = Vec::with_capacity(loops.len());
        inner_loops.push(offset_contour(&loops[0], chamfer).unwrap_or_else(|_| loops[0].clone()));
        inner_loops.extend(holes.iter().cloned());
        let lower_footprint = push_layer(&mut mesh, &loops, z0 + chamfer);
        let upper_footprint = push_layer(&mut mesh, &loops, z1 - chamfer);
        let bottom_inner = push_layer(&mut mesh, &inner_loops, z0);
        let top_inner = push_layer(&mut mesh, &inner_loops, z1);
        let cap_indices = triangulate_loops(&inner_loops)?;
        let cap_boundary_edges = boundary_edges_from_indices(&cap_indices);
        if !boundary_edges_form_loops(&cap_boundary_edges, &inner_loops) {
            return extrude_groups(groups, z0, z1);
        }
        add_cap_triangles(&mut mesh, &bottom_inner, &cap_indices, false);
        add_cap_triangles(&mut mesh, &top_inner, &cap_indices, true);
        add_loop_rings(
            &mut mesh,
            &lower_footprint,
            &upper_footprint,
            &cap_boundary_edges,
        );
        add_loop_rings(
            &mut mesh,
            &bottom_inner,
            &lower_footprint,
            &cap_boundary_edges,
        );
        add_loop_rings(&mut mesh, &upper_footprint, &top_inner, &cap_boundary_edges);
    }
    Ok(mesh)
}

fn common_resampled_contours(contours: &[&Contour]) -> Vec<Contour> {
    let mut fractions = Vec::new();
    for contour in contours {
        fractions.extend(contour_vertex_fractions(contour));
    }
    fractions.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    fractions.dedup_by(|a, b| (*a - *b).abs() <= 1e-10);
    if fractions.is_empty() {
        fractions.push(0.0);
    }
    contours
        .iter()
        .map(|contour| resample_contour(contour, &fractions))
        .collect()
}

fn contour_vertex_fractions(contour: &Contour) -> Vec<f64> {
    if contour.is_empty() {
        return Vec::new();
    }
    let perimeter = contour
        .iter()
        .enumerate()
        .map(|(i, point)| point.distance(contour[(i + 1) % contour.len()]))
        .sum::<f64>();
    if perimeter <= 1e-12 {
        return vec![0.0];
    }
    let mut fractions = Vec::with_capacity(contour.len());
    let mut distance = 0.0;
    for (i, point) in contour.iter().enumerate() {
        if i == 0 {
            fractions.push(0.0);
        } else {
            fractions.push(distance / perimeter);
        }
        distance += point.distance(contour[(i + 1) % contour.len()]);
    }
    fractions
}

fn resample_contour(contour: &Contour, fractions: &[f64]) -> Contour {
    if contour.len() < 2 || fractions.len() <= 1 {
        return contour.clone();
    }
    let perimeter = contour
        .iter()
        .enumerate()
        .map(|(i, point)| point.distance(contour[(i + 1) % contour.len()]))
        .sum::<f64>();
    if perimeter <= 1e-12 {
        return contour.clone();
    }
    fractions
        .iter()
        .map(|fraction| {
            let target = fraction.clamp(0.0, 1.0 - 1e-12) * perimeter;
            let mut travelled = 0.0;
            for (i, start) in contour.iter().enumerate() {
                let end = contour[(i + 1) % contour.len()];
                let length = start.distance(end);
                if length <= 1e-12 {
                    continue;
                }
                if target <= travelled + length || i == contour.len() - 1 {
                    let t = ((target - travelled) / length).clamp(0.0, 1.0);
                    return Point::new(
                        start.x + (end.x - start.x) * t,
                        start.y + (end.y - start.y) * t,
                    );
                }
                travelled += length;
            }
            contour[0]
        })
        .collect()
}

fn perturb_collinear_points(contour: &mut Contour) {
    if contour.len() < 3 {
        return;
    }
    let original = contour.clone();
    let epsilon = COLLINEAR_REPAIR_EPSILON;
    for i in 0..contour.len() {
        let previous = original[(i + original.len() - 1) % original.len()];
        let current = original[i];
        let next = original[(i + 1) % original.len()];
        let dx = next.x - previous.x;
        let dy = next.y - previous.y;
        let length = (dx * dx + dy * dy).sqrt();
        let area = (current.x - previous.x) * dy - (current.y - previous.y) * dx;
        if length <= 1e-12 || area.abs() > 1e-10 {
            continue;
        }
        let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
        contour[i].x += -dy / length * epsilon * sign;
        contour[i].y += dx / length * epsilon * sign;
    }
}

fn chamfer_contour(contour: &Contour, distance: f64) -> Contour {
    if contour.len() < 3 || distance <= 0.0 {
        return contour.clone();
    }
    let orientation = signed_area(contour).signum();
    if orientation == 0.0 {
        return contour.clone();
    }
    let mut result = Vec::with_capacity(contour.len() * 2);
    for i in 0..contour.len() {
        let prev = contour[(i + contour.len() - 1) % contour.len()];
        let current = contour[i];
        let next = contour[(i + 1) % contour.len()];
        let prev_len = current.distance(prev);
        let next_len = current.distance(next);
        if prev_len <= 1e-9 || next_len <= 1e-9 {
            result.push(current);
            continue;
        }
        let turn = (current.x - prev.x) * (next.y - current.y)
            - (current.y - prev.y) * (next.x - current.x);
        let a = Point::new(
            (prev.x - current.x) / prev_len,
            (prev.y - current.y) / prev_len,
        );
        let b = Point::new(
            (next.x - current.x) / next_len,
            (next.y - current.y) / next_len,
        );
        let angle = (a.x * b.x + a.y * b.y).clamp(-1.0, 1.0).acos();
        let is_sharp_convex =
            turn * orientation > 1e-9 && angle < 170.0_f64.to_radians() && angle > 1e-6;
        if !is_sharp_convex {
            result.push(current);
            continue;
        }
        let cut = distance.min(prev_len.min(next_len) * 0.49);
        result.push(Point::new(current.x + a.x * cut, current.y + a.y * cut));
        result.push(Point::new(current.x + b.x * cut, current.y + b.y * cut));
    }
    result
}

fn offset_contour(contour: &Contour, distance: f64) -> Result<Contour> {
    if contour.len() < 3 || distance <= 0.0 {
        return Ok(contour.clone());
    }
    let material_side = signed_area(contour).signum();
    if material_side == 0.0 {
        return Err(anyhow!("cannot offset a zero-area contour"));
    }
    let mut result = Vec::with_capacity(contour.len());
    for i in 0..contour.len() {
        let prev = contour[(i + contour.len() - 1) % contour.len()];
        let current = contour[i];
        let next = contour[(i + 1) % contour.len()];
        let first = shifted_line(prev, current, material_side * distance)?;
        let second = shifted_line(current, next, material_side * distance)?;
        result.push(line_intersection(first, second)?);
    }
    let area = signed_area(&result);
    if !area.is_finite() || area.abs() <= 1e-9 || area.signum() != material_side {
        return Err(anyhow!("contour offset is invalid"));
    }
    Ok(result)
}

fn shifted_line(a: Point, b: Point, distance: f64) -> Result<(Point, Point)> {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= 1e-9 {
        return Err(anyhow!("contour contains a zero-length edge"));
    }
    let normal = Point::new(-dy / length * distance, dx / length * distance);
    Ok((
        Point::new(a.x + normal.x, a.y + normal.y),
        Point::new(b.x + normal.x, b.y + normal.y),
    ))
}

fn line_intersection(first: (Point, Point), second: (Point, Point)) -> Result<Point> {
    let (a, b) = first;
    let (c, d) = second;
    let r = Point::new(b.x - a.x, b.y - a.y);
    let s = Point::new(d.x - c.x, d.y - c.y);
    let denominator = r.x * s.y - r.y * s.x;
    if denominator.abs() <= 1e-10 {
        return Err(anyhow!("parallel contour offset edges"));
    }
    let q_minus_a = Point::new(c.x - a.x, c.y - a.y);
    let t = (q_minus_a.x * s.y - q_minus_a.y * s.x) / denominator;
    Ok(Point::new(a.x + t * r.x, a.y + t * r.y))
}

fn push_layer(mesh: &mut Mesh, loops: &[Contour], z: f64) -> Vec<u32> {
    let mut offsets = Vec::with_capacity(loops.len());
    for contour in loops {
        offsets.push(mesh.vertices.len() as u32);
        for p in contour {
            mesh.vertices.push(Vertex { x: p.x, y: p.y, z });
        }
    }
    offsets
}

fn triangulate_loops(loops: &[Contour]) -> Result<Vec<usize>> {
    let mut coords = Vec::new();
    let mut hole_indices = Vec::new();
    let mut count = 0;
    for (li, contour) in loops.iter().enumerate() {
        if li > 0 {
            hole_indices.push(count);
        }
        count += contour.len();
        for p in contour {
            coords.push(p.x);
            coords.push(p.y);
        }
    }
    earcutr::earcut(&coords, &hole_indices, 2).map_err(|e| anyhow!("triangulation failed: {e:?}"))
}

fn add_cap_triangles(mesh: &mut Mesh, offsets: &[u32], indices: &[usize], top: bool) {
    for tri in indices.chunks_exact(3) {
        let a = vertex_index(offsets, tri[0]);
        let b = vertex_index(offsets, tri[1]);
        let c = vertex_index(offsets, tri[2]);
        mesh.triangles.push(if top {
            Triangle {
                v1: a,
                v2: b,
                v3: c,
            }
        } else {
            Triangle {
                v1: c,
                v2: b,
                v3: a,
            }
        });
    }
}

fn boundary_edges_from_indices(indices: &[usize]) -> Vec<(usize, usize)> {
    let mut edges: HashMap<(usize, usize), (usize, (usize, usize))> = HashMap::new();
    for tri in indices.chunks_exact(3) {
        for &(a, b) in &[(tri[0], tri[1]), (tri[1], tri[2]), (tri[2], tri[0])] {
            let key = (a.min(b), a.max(b));
            let entry = edges.entry(key).or_insert((0, (a, b)));
            entry.0 += 1;
        }
    }
    edges
        .into_values()
        .filter_map(|(count, directed)| (count == 1).then_some(directed))
        .collect()
}

fn boundary_edges_form_loops(edges: &[(usize, usize)], loops: &[Contour]) -> bool {
    let mut degrees: HashMap<usize, usize> = HashMap::new();
    for &(a, b) in edges {
        *degrees.entry(a).or_default() += 1;
        *degrees.entry(b).or_default() += 1;
    }
    let expected = loops.iter().map(|contour| contour.len()).sum::<usize>();
    degrees.len() == expected && degrees.values().all(|degree| *degree == 2)
}

fn vertex_index(offsets: &[u32], flat_index: usize) -> u32 {
    let Some(first) = offsets.first().copied() else {
        return flat_index as u32;
    };
    let absolute = first as usize + flat_index;
    for i in 0..offsets.len() {
        let next = offsets
            .get(i + 1)
            .copied()
            .map(|value| value as usize)
            .unwrap_or(usize::MAX);
        if absolute < next {
            return absolute as u32;
        }
    }
    absolute as u32
}

fn add_loop_rings(mesh: &mut Mesh, lower: &[u32], upper: &[u32], edges: &[(usize, usize)]) {
    let Some(lower_base) = lower.first().copied() else {
        return;
    };
    let Some(upper_base) = upper.first().copied() else {
        return;
    };
    for &(flat_a, flat_b) in edges {
        let a = lower_base + flat_a as u32;
        let b = lower_base + flat_b as u32;
        let at = upper_base + flat_a as u32;
        let bt = upper_base + flat_b as u32;
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

fn repair_degenerate_cap_across_layers(mesh: &mut Mesh, offsets: &[Vec<u32>], indices: &[usize]) {
    for layer in 0..offsets.len() {
        for tri in indices.chunks_exact(3) {
            let flat = [tri[0], tri[1], tri[2]];
            let vertex_indices = flat.map(|index| vertex_index(&offsets[layer], index));
            let points = vertex_indices.map(|index| mesh.vertices[index as usize]);
            if triangle_area2(points[0], points[1], points[2]).abs() > DEGENERATE_AREA2_THRESHOLD {
                continue;
            }
            let distance =
                |a: Vertex, b: Vertex| ((a.x - b.x).powi(2) + (a.y - b.y).powi(2)).sqrt();
            let mut middle = 0;
            let mut best = f64::INFINITY;
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
                continue;
            }
            let nx = -dy / length * COLLINEAR_REPAIR_EPSILON;
            let ny = dx / length * COLLINEAR_REPAIR_EPSILON;
            for layer_offsets in offsets {
                let index = vertex_index(layer_offsets, flat[middle]);
                mesh.vertices[index as usize].x += nx;
                mesh.vertices[index as usize].y += ny;
            }
        }
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

    #[test]
    fn chamfered_rectangle_with_hole_is_closed() {
        let outer = vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 50.0),
            Point::new(0.0, 50.0),
        ];
        let hole = vec![
            Point::new(40.0, 20.0),
            Point::new(40.0, 30.0),
            Point::new(60.0, 30.0),
            Point::new(60.0, 20.0),
        ];
        let mesh =
            extrude_groups_chamfered(&[(outer, vec![hole])], 0.0, 3.0, 0.2, true, true).unwrap();
        validate_parts(&[Part {
            name: "chamfered".into(),
            color: Rgb(0, 0, 0),
            mesh,
            is_backing: true,
            region_index: 0,
        }])
        .unwrap();
    }

    #[test]
    fn backed_chamfer_stays_on_exposed_outer_faces() {
        let outer = vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 50.0),
            Point::new(0.0, 50.0),
        ];
        let backing =
            extrude_groups_chamfered(&[(outer.clone(), Vec::new())], 0.0, 2.0, 1.2, true, false)
                .unwrap();
        let face =
            extrude_groups_chamfered(&[(outer.clone(), Vec::new())], 2.0, 3.0, 1.2, false, true)
                .unwrap();
        validate_parts(&[
            Part {
                name: "backing".into(),
                color: Rgb(0, 0, 0),
                mesh: backing.clone(),
                is_backing: true,
                region_index: usize::MAX,
            },
            Part {
                name: "face".into(),
                color: Rgb(255, 255, 255),
                mesh: face.clone(),
                is_backing: false,
                region_index: 0,
            },
        ])
        .unwrap();
        assert!(backing
            .vertices
            .iter()
            .any(|v| (v.z - 2.0).abs() < 1e-9 && v.x.abs() < 1e-9 && v.y.abs() < 1e-9));
        assert!(face
            .vertices
            .iter()
            .any(|v| (v.z - 2.0).abs() < 1e-9 && v.x.abs() < 1e-9 && v.y.abs() < 1e-9));
    }

    #[test]
    fn fillet_rounds_a_square_outline() {
        let square = vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 50.0),
            Point::new(0.0, 50.0),
        ];
        let rounded = fillet_contour(&square, 5.0).unwrap();
        assert!(rounded.len() > square.len());
        let bounds = contour_bounds(std::slice::from_ref(&rounded)).unwrap();
        assert!((bounds.0 - 0.0).abs() < 1e-9);
        assert!((bounds.1 - 0.0).abs() < 1e-9);
        assert!((bounds.2 - 100.0).abs() < 1e-9);
        assert!((bounds.3 - 50.0).abs() < 1e-9);
        assert!(signed_area(&rounded) < signed_area(&square));
    }

    #[test]
    fn countersunk_hole_is_a_closed_tapered_shell() {
        let outer = vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
            Point::new(0.0, 100.0),
        ];
        let hole = (0..32)
            .map(|i| {
                let angle = std::f64::consts::TAU * i as f64 / 32.0;
                Point::new(50.0 + 2.5 * angle.cos(), 50.0 + 2.5 * angle.sin())
            })
            .collect();
        let mesh = extrude_groups_countersunk(
            &[(outer, vec![hole])],
            0.0,
            3.0,
            &[CountersinkHole {
                center: Point::new(50.0, 50.0),
                bore_radius: 2.5,
                opening_radius: 4.0,
                depth: 1.0,
            }],
            true,
        )
        .unwrap();
        validate_parts(&[Part {
            name: "countersunk".into(),
            color: Rgb(0, 0, 0),
            mesh: mesh.clone(),
            is_backing: true,
            region_index: 0,
        }])
        .unwrap();
        assert!(mesh.vertices.iter().any(|vertex| {
            (vertex.z - 3.0).abs() < 1e-9
                && (vertex.x - 50.0).abs() > 3.5
                && (vertex.x - 50.0).abs() < 4.1
        }));
    }

    #[test]
    fn countersink_and_exposed_chamfer_are_closed_together() {
        let outer = vec![
            Point::new(0.0, 0.0),
            Point::new(100.0, 0.0),
            Point::new(100.0, 100.0),
            Point::new(0.0, 100.0),
        ];
        let hole = (0..32)
            .map(|i| {
                let angle = std::f64::consts::TAU * i as f64 / 32.0;
                Point::new(50.0 + 2.5 * angle.cos(), 50.0 + 2.5 * angle.sin())
            })
            .collect();
        let mesh = extrude_groups_countersunk_chamfered(
            &[(outer, vec![hole])],
            0.0,
            3.0,
            &[CountersinkHole {
                center: Point::new(50.0, 50.0),
                bore_radius: 2.5,
                opening_radius: 4.0,
                depth: 1.0,
            }],
            true,
            0.2,
            false,
            true,
        )
        .unwrap();
        validate_parts(&[Part {
            name: "countersunk-chamfered".into(),
            color: Rgb(0, 0, 0),
            mesh,
            is_backing: false,
            region_index: 0,
        }])
        .unwrap();
    }

    #[test]
    fn countersunk_extrusion_handles_many_contours() {
        let outer = (0..100)
            .map(|i| {
                let t = i as f64 / 100.0;
                Point::new(100.0 * t, if i < 50 { 0.0 } else { 50.0 })
            })
            .collect::<Vec<_>>();
        let mut holes = Vec::new();
        for center in [Point::new(20.0, 25.0), Point::new(80.0, 25.0)] {
            holes.push(
                (0..32)
                    .map(|i| {
                        let angle = std::f64::consts::TAU * i as f64 / 32.0;
                        Point::new(center.x + 2.5 * angle.cos(), center.y + 2.5 * angle.sin())
                    })
                    .collect(),
            );
        }
        let mesh = extrude_groups_countersunk(
            &[(outer, holes)],
            0.0,
            3.0,
            &[CountersinkHole {
                center: Point::new(20.0, 25.0),
                bore_radius: 2.5,
                opening_radius: 4.0,
                depth: 1.0,
            }],
            true,
        )
        .unwrap();
        validate_parts(&[Part {
            name: "many-contours".into(),
            color: Rgb(0, 0, 0),
            mesh,
            is_backing: true,
            region_index: 0,
        }])
        .unwrap();
    }
}
