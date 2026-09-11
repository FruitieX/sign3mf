use crate::ensure_parent;
use crate::mesh::Part;
use anyhow::{Context, Result};
use std::path::Path;

pub fn write_svg(path: &Path, parts: &[Part]) -> Result<()> {
    ensure_parent(path)?;
    let (w, h) = crate::mesh::bounds(parts);
    let mut s = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?><svg xmlns="http://www.w3.org/2000/svg" width="{w:.4}mm" height="{h:.4}mm" viewBox="0 0 {w:.9} {h:.9}"><title>sign3mf</title>"#
    );
    for part in parts {
        if part.is_backing {
            continue;
        }
        let top = part
            .mesh
            .vertices
            .iter()
            .map(|v| v.z)
            .fold(f64::NEG_INFINITY, f64::max);
        for tri in &part.mesh.triangles {
            let a = part.mesh.vertices[tri.v1 as usize];
            let b = part.mesh.vertices[tri.v2 as usize];
            let c = part.mesh.vertices[tri.v3 as usize];
            if (a.z - top).abs() > 1e-7 || (b.z - top).abs() > 1e-7 || (c.z - top).abs() > 1e-7 {
                continue;
            }
            // Mesh coordinates use Y-up; SVG uses Y-down.
            let ay = h - a.y;
            let by = h - b.y;
            let cy = h - c.y;
            s.push_str(&format!(
                r#"<path d="M {:.5},{:.5} L {:.5},{:.5} L {:.5},{:.5} Z" fill="{}" data-part="{}"/>"#,
                a.x, ay, b.x, by, c.x, cy, part.color.html(), part.name
            ));
        }
    }
    s.push_str("</svg>");
    std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}
