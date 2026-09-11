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
    zip.start_file("Metadata/model_settings.config", opts)?;
    zip.write_all(model_settings_xml(parts).as_bytes())?;
    zip.finish()?;
    Ok(())
}

fn content_types() -> &'static str {
    r#"<?xml version="1.0" encoding="UTF-8"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/><Default Extension="config" ContentType="application/xml"/></Types>"#
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
            r#"<object id="{}" type="model" name="{}" pid="1" pindex="{}"><mesh><vertices>"#,
            id,
            esc(&p.name),
            i
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

fn model_settings_xml(parts: &[Part]) -> String {
    let mut colors = Vec::new();
    let mut extruders = Vec::with_capacity(parts.len());
    for part in parts {
        let index = colors
            .iter()
            .position(|color| *color == part.color)
            .unwrap_or_else(|| {
                colors.push(part.color);
                colors.len() - 1
            });
        extruders.push(index + 1);
    }

    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><config><object id=\"1\"><metadata key=\"name\" value=\"sign3d\"/>",
    );
    for (i, part) in parts.iter().enumerate() {
        out.push_str(&format!(
            "<part id=\"{}\" subtype=\"normal_part\"><metadata key=\"name\" value=\"{}\"/><metadata key=\"extruder\" value=\"{}\"/></part>",
            i + 2,
            esc(&part.name),
            extruders[i]
        ));
    }
    out.push_str("</object><plate><metadata key=\"plater_id\" value=\"1\"/><metadata key=\"filament_map_mode\" value=\"Auto For Flush\"/><metadata key=\"filament_maps\" value=\"");
    for extruder in 1..=colors.len() {
        if extruder > 1 {
            out.push(' ');
        }
        out.push_str(&extruder.to_string());
    }
    out.push_str("\"/><model_instance><metadata key=\"object_id\" value=\"1\"/><metadata key=\"instance_id\" value=\"0\"/></model_instance></plate></config>");
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

    #[test]
    fn model_materials_and_slicer_assignments_are_distinct() {
        let parts = vec![
            Part {
                name: "backing_0000FF".into(),
                color: crate::Rgb(0, 0, 255),
                mesh: crate::Mesh::default(),
                is_backing: true,
                region_index: 0,
            },
            Part {
                name: "color_01_0000FF".into(),
                color: crate::Rgb(0, 0, 255),
                mesh: crate::Mesh::default(),
                is_backing: false,
                region_index: 0,
            },
            Part {
                name: "color_02_FFFFFF".into(),
                color: crate::Rgb(255, 255, 255),
                mesh: crate::Mesh::default(),
                is_backing: false,
                region_index: 1,
            },
        ];
        let config = model_settings_xml(&parts);
        assert!(
            config.contains("<part id=\"2\" subtype=\"normal_part\">")
                && config.contains("<metadata key=\"extruder\" value=\"1\"/>")
        );
        assert!(
            config.contains("<part id=\"4\" subtype=\"normal_part\">")
                && config.contains("<metadata key=\"extruder\" value=\"2\"/>")
        );
        assert!(config.contains("key=\"filament_maps\" value=\"1 2\""));
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
