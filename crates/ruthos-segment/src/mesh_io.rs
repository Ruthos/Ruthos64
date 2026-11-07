//! Writing a surface as STL, OBJ, PLY or 3MF.
//!
//! Every format carries a note of the scan it came from where the format has
//! room for one. PLY and 3MF also carry the segment's colour. Triangles are
//! written turning anticlockwise seen from outside, which 3MF requires and
//! printers expect of the others.

use std::io::Write;
use std::path::Path;

use crate::mesh::Mesh;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MeshFormat {
    Stl,
    Obj,
    Ply,
    ThreeMf,
}

impl MeshFormat {
    pub const ALL: [MeshFormat; 4] = [Self::Stl, Self::Obj, Self::Ply, Self::ThreeMf];

    pub fn extension(self) -> &'static str {
        match self {
            Self::Stl => "stl",
            Self::Obj => "obj",
            Self::Ply => "ply",
            Self::ThreeMf => "3mf",
        }
    }

    /// For a menu: the name and what it suits.
    pub fn label(self) -> &'static str {
        match self {
            Self::Stl => "STL, for any printer",
            Self::Obj => "OBJ, for 3D software",
            Self::Ply => "PLY, with its colour",
            Self::ThreeMf => "3MF, for modern printers, with its colour",
        }
    }

    /// The format a file name asks for, by its extension.
    pub fn of(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        Self::ALL.into_iter().find(|f| f.extension() == ext)
    }
}

impl Mesh {
    /// Turn every triangle the other way if the mesh is inside out, as it is
    /// after a transform that mirrors, such as a series stored feet first.
    pub fn orient_outward(&mut self) {
        if self.volume() < 0.0 {
            for t in &mut self.triangles {
                t.swap(1, 2);
            }
        }
    }

    /// The mesh in `format`. `name` and `colour` are the segment's.
    pub fn encode(&self, format: MeshFormat, note: &str, name: &str, colour: [u8; 3]) -> Result<Vec<u8>, String> {
        Ok(match format {
            MeshFormat::Stl => self.to_stl(note),
            MeshFormat::Obj => self.to_obj(note, name),
            MeshFormat::Ply => self.to_ply(note, colour),
            MeshFormat::ThreeMf => self.to_3mf(note, name, colour)?,
        })
    }

    /// Wavefront OBJ: text, one line a vertex and one a triangle.
    pub fn to_obj(&self, note: &str, name: &str) -> Vec<u8> {
        let mut out = String::with_capacity(self.vertices.len() * 40 + self.triangles.len() * 24);
        for line in note.lines() {
            out.push_str(&format!("# {line}\n"));
        }
        out.push_str(&format!("o {}\n", one_line(name)));
        for v in &self.vertices {
            out.push_str(&format!("v {} {} {}\n", v[0] as f32, v[1] as f32, v[2] as f32));
        }
        for t in &self.triangles {
            out.push_str(&format!("f {} {} {}\n", t[0] + 1, t[1] + 1, t[2] + 1));
        }
        out.into_bytes()
    }

    /// Binary PLY, each vertex coloured.
    pub fn to_ply(&self, note: &str, colour: [u8; 3]) -> Vec<u8> {
        let mut out = Vec::with_capacity(256 + self.vertices.len() * 15 + self.triangles.len() * 13);
        let mut header = String::from("ply\nformat binary_little_endian 1.0\n");
        for line in note.lines() {
            header.push_str(&format!("comment {line}\n"));
        }
        header.push_str(&format!(
            "element vertex {}\nproperty float x\nproperty float y\nproperty float z\n\
             property uchar red\nproperty uchar green\nproperty uchar blue\n\
             element face {}\nproperty list uchar int vertex_indices\nend_header\n",
            self.vertices.len(),
            self.triangles.len()
        ));
        out.extend_from_slice(header.as_bytes());
        for v in &self.vertices {
            for c in v {
                out.extend_from_slice(&(*c as f32).to_le_bytes());
            }
            out.extend_from_slice(&colour);
        }
        for t in &self.triangles {
            out.push(3);
            for i in t {
                out.extend_from_slice(&(*i as i32).to_le_bytes());
            }
        }
        out
    }

    /// 3MF: a zip holding the model as XML, in millimetres, with its colour.
    pub fn to_3mf(&self, note: &str, name: &str, colour: [u8; 3]) -> Result<Vec<u8>, String> {
        let mut model = String::with_capacity(self.vertices.len() * 60 + self.triangles.len() * 50 + 1024);
        model.push_str(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <model unit=\"millimeter\" xml:lang=\"en-GB\" \
             xmlns=\"http://schemas.microsoft.com/3dmanufacturing/core/2015/02\">\n",
        );
        model.push_str(&format!(" <metadata name=\"Title\">{}</metadata>\n", xml(name)));
        model.push_str(&format!(" <metadata name=\"Description\">{}</metadata>\n", xml(note)));
        model.push_str(&format!(
            " <metadata name=\"Application\">Ruthos {}</metadata>\n",
            env!("CARGO_PKG_VERSION")
        ));
        model.push_str(" <resources>\n");
        model.push_str(&format!(
            "  <basematerials id=\"1\"><base name=\"{}\" displaycolor=\"#{:02X}{:02X}{:02X}\"/></basematerials>\n",
            xml(name),
            colour[0],
            colour[1],
            colour[2]
        ));
        model.push_str(&format!(
            "  <object id=\"2\" type=\"model\" pid=\"1\" pindex=\"0\" name=\"{}\">\n   <mesh>\n    <vertices>\n",
            xml(name)
        ));
        for v in &self.vertices {
            model.push_str(&format!(
                "     <vertex x=\"{}\" y=\"{}\" z=\"{}\"/>\n",
                v[0] as f32, v[1] as f32, v[2] as f32
            ));
        }
        model.push_str("    </vertices>\n    <triangles>\n");
        for t in &self.triangles {
            model.push_str(&format!(
                "     <triangle v1=\"{}\" v2=\"{}\" v3=\"{}\"/>\n",
                t[0], t[1], t[2]
            ));
        }
        model.push_str("    </triangles>\n   </mesh>\n  </object>\n </resources>\n <build><item objectid=\"2\"/></build>\n</model>\n");

        let fail = |e: zip::result::ZipError| e.to_string();
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        for (path, text) in [
            (
                "[Content_Types].xml",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
                 <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
                 <Default Extension=\"model\" ContentType=\"application/vnd.ms-package.3dmanufacturing-3dmodel+xml\"/>\
                 </Types>\n",
            ),
            (
                "_rels/.rels",
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
                 <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
                 <Relationship Target=\"/3D/3dmodel.model\" Id=\"rel0\" \
                 Type=\"http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel\"/>\
                 </Relationships>\n",
            ),
            ("3D/3dmodel.model", model.as_str()),
        ] {
            zip.start_file(path, options).map_err(fail)?;
            zip.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
        }
        Ok(zip.finish().map_err(fail)?.into_inner())
    }
}

fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

fn xml(text: &str) -> String {
    one_line(text)
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::{sphere_mask, surface_nets};
    use crate::Grid;
    use std::io::Read;

    fn ball() -> Mesh {
        let g = Grid {
            dims: [12, 12, 12],
            spacing: [1.0, 1.0, 1.0],
        };
        surface_nets(&sphere_mask(g, 4.0))
    }

    #[test]
    fn a_mirrored_mesh_is_turned_back_outward() {
        let mut mesh = ball();
        let volume = mesh.volume();
        mesh.to_patient([0.0; 3], [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, -1.0]]);
        assert!(mesh.volume() < 0.0, "a mirror turns it inside out");
        mesh.orient_outward();
        assert!((mesh.volume() - volume).abs() < 1e-6);
    }

    #[test]
    fn obj_counts_from_one() {
        let mesh = ball();
        let text = String::from_utf8(mesh.to_obj("Series 3\nPlanning only", "Left <femur>")).unwrap();
        assert!(text.starts_with("# Series 3\n# Planning only\no Left <femur>\n"));
        assert_eq!(
            text.lines().filter(|l| l.starts_with("v ")).count(),
            mesh.vertices.len()
        );
        assert_eq!(
            text.lines().filter(|l| l.starts_with("f ")).count(),
            mesh.triangles.len()
        );
        let highest = text
            .lines()
            .filter_map(|l| l.strip_prefix("f "))
            .flat_map(|l| l.split(' ').map(|n| n.parse::<usize>().unwrap()))
            .max()
            .unwrap();
        assert_eq!(highest, mesh.vertices.len());
    }

    #[test]
    fn ply_is_the_size_its_header_says() {
        let mesh = ball();
        let ply = mesh.to_ply("Series 3", [200, 180, 150]);
        let end = ply.windows(11).position(|w| w == b"end_header\n").unwrap() + 11;
        let header = std::str::from_utf8(&ply[..end]).unwrap();
        assert!(header.contains(&format!("element vertex {}", mesh.vertices.len())));
        assert!(header.contains("comment Series 3"));
        assert_eq!(ply.len() - end, mesh.vertices.len() * 15 + mesh.triangles.len() * 13);
        assert_eq!(&ply[end + 12..end + 15], &[200, 180, 150]);
    }

    #[test]
    fn three_mf_is_a_package_with_the_model() {
        let mesh = ball();
        let bytes = mesh.to_3mf("Series 3 & 4", "Bone", [255, 128, 0]).unwrap();
        let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        for part in ["[Content_Types].xml", "_rels/.rels"] {
            assert!(zip.by_name(part).is_ok(), "{part}");
        }
        let mut model = String::new();
        zip.by_name("3D/3dmodel.model")
            .unwrap()
            .read_to_string(&mut model)
            .unwrap();
        assert!(model.contains("unit=\"millimeter\""));
        assert!(model.contains("Series 3 &amp; 4"));
        assert!(model.contains("displaycolor=\"#FF8000\""));
        assert_eq!(model.matches("<vertex ").count(), mesh.vertices.len());
        assert_eq!(model.matches("<triangle ").count(), mesh.triangles.len());
    }

    #[test]
    fn a_format_comes_from_the_file_name() {
        assert_eq!(MeshFormat::of(Path::new("a/b.3MF")), Some(MeshFormat::ThreeMf));
        assert_eq!(MeshFormat::of(Path::new("bone.ply")), Some(MeshFormat::Ply));
        assert_eq!(MeshFormat::of(Path::new("bone")), None);
    }
}
