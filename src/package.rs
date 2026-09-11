use std::fs::File;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use zip::write::FileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::ensure_parent;
use crate::mesh::Part;

pub fn write_3mf(path: &Path, parts: &[Part]) -> Result<()> {
    ensure_parent(path)?;
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut zip = ZipWriter::new(file);
    let opts = FileOptions::<()>::default().compression_method(CompressionMethod::Deflated);
    zip.start_file("[Content_Types].xml", opts)?;
    zip.write_all(content_types().as_bytes())?;
    zip.start_file("_rels/.rels", opts)?;
    zip.write_all(root_rels().as_bytes())?;
    zip.start_file("3D/3dmodel.model", opts)?;
    zip.write_all(model_xml(parts).as_bytes())?;
    zip.finish()?;
    Ok(())
}

fn content_types() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/></Types>"#
}
fn root_rels() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>"#
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn model_xml(parts: &[Part]) -> String {
    let mut out = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<model unit="millimeter" xml:lang="en-US" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02" xmlns:m="http://schemas.microsoft.com/3dmanufacturing/material/2015/02"><resources>"#,
    );
    out.push_str(r#"<basematerials id="1">"#);
    for p in parts {
        out.push_str(&format!(
            r#"<base name="{}" displaycolor="{}"/>"#,
            esc(&p.name),
            p.color.html()
        ));
    }
    out.push_str("</basematerials>");
    for (i, p) in parts.iter().enumerate() {
        let id = i + 2;
        out.push_str(&format!(
            r#"<object id="{}" type="model" name="{}"><mesh><vertices>"#,
            id,
            esc(&p.name)
        ));
        for v in &p.mesh.vertices {
            out.push_str(&format!(
                r#"<vertex x="{:.9}" y="{:.9}" z="{:.9}"/>"#,
                v.x, v.y, v.z
            ));
        }
        out.push_str("</vertices><triangles>");
        for t in &p.mesh.triangles {
            out.push_str(&format!(
                r#"<triangle v1="{}" v2="{}" v3="{}" pid="1" p1="{}"/>"#,
                t.v1, t.v2, t.v3, i
            ));
        }
        out.push_str("</triangles></mesh></object>");
    }
    out.push_str(r#"<object id="1" type="model" name="sign3d"><components>"#);
    for i in 0..parts.len() {
        out.push_str(&format!(r#"<component objectid="{}"/>"#, i + 2));
    }
    out.push_str("</components></object></resources><build><item objectid=\"1\"/></build></model>");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xml_has_required_parts() {
        assert!(content_types().contains("3dmodel"));
        assert!(root_rels().contains("3dmodel.model"));
    }

    #[cfg(feature = "lib3mf-backend")]
    #[test]
    fn lib3mf_round_trip_reads_component_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("roundtrip.3mf");
        let part = Part {
            name: "color_01_FF0000".into(),
            color: crate::Rgb(255, 0, 0),
            mesh: crate::Mesh {
                vertices: vec![
                    crate::Vertex {
                        x: 0.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    crate::Vertex {
                        x: 1.0,
                        y: 0.0,
                        z: 0.0,
                    },
                    crate::Vertex {
                        x: 0.0,
                        y: 1.0,
                        z: 0.0,
                    },
                ],
                triangles: vec![crate::Triangle {
                    v1: 0,
                    v2: 1,
                    v3: 2,
                }],
            },
            is_backing: false,
            region_index: 0,
        };
        write_3mf(&path, &[part]).unwrap();
        let model = lib3mf::Model::from_reader(std::fs::File::open(path).unwrap()).unwrap();
        assert_eq!(model.resources.objects.len(), 2);
        assert_eq!(model.build.items.len(), 1);
    }
}
