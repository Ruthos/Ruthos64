//! Measurement definitions, sets and calculations.
//!
//! A *definition* names one thing that can be measured (`lv.lvidd`, "LVIDd",
//! a length from the LV M-mode tool, in mm). A *set* groups definitions for
//! a kind of study. A *calculation* derives a number from definitions with a
//! small expression language: numbers, definition keys, `+ − × ÷`, powers,
//! parentheses and `sqrt`, `abs`, `min`, `max`.
//!
//! Ruthos ships a built-in library that cannot be changed in place. A user
//! may duplicate an entry under a new key, edit a copy under the same key
//! (which then stands in for the built-in one) and restore the built-in by
//! deleting the copy. The user's entries live in settings.json; JSON files
//! can be exported and imported, merging by key.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------- model

/// One measurable quantity.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Definition {
    /// Stable identifier, lower case, dotted: `lv.lvidd`, `vhs.long`.
    pub key: String,
    /// What the clinician sees: `LVIDd`.
    pub label: String,
    /// The tool that produces it: `caliper`, `angle`, `area`, `point` or
    /// `lv-mmode` (`length` is accepted for `caliper`).
    pub tool: String,
    /// What is recorded when a measurement is given this name:
    /// `length`, `time`, `velocity`, `slope`, `pht` (pressure half-time),
    /// `angle`, `area` or `value`. One caliper line can give any of them.
    pub quantity: String,
    /// Stored unit: `mm`, `deg`, `mm2` or empty.
    pub unit: String,
    pub decimals: u8,
    /// Optional coded concept, e.g. `LN:29436-3` or `SCT:396279008`.
    pub concept: Option<String>,
    /// Species the definition applies to, when not general.
    pub species: Option<String>,
}

impl Default for Definition {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            tool: "caliper".into(),
            quantity: "length".into(),
            unit: "mm".into(),
            decimals: 1,
            concept: None,
            species: None,
        }
    }
}

/// A named group of definitions, by key.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Set {
    pub name: String,
    pub keys: Vec<String>,
    /// The kinds of study it is for. Empty: inferred from the name (sets
    /// saved before regions existed), and a set with no region at all is
    /// offered everywhere.
    pub regions: Vec<Region>,
}

impl Set {
    /// The regions this set is for, given or inferred from its name.
    pub fn regions(&self) -> Vec<Region> {
        if !self.regions.is_empty() {
            return self.regions.clone();
        }
        let n = self.name.to_ascii_lowercase();
        let mut out = Vec::new();
        if ["echo", "cardi", "heart", "vhs", "doppler"]
            .iter()
            .any(|w| n.contains(w))
        {
            out.push(Region::Cardiac);
        }
        if ["abdom", "kidney", "renal", "liver", "spleen", "bladder"]
            .iter()
            .any(|w| n.contains(w))
        {
            out.push(Region::Abdominal);
        }
        if ["ortho", "tendon", "ligament", "joint", "limb", "msk", "musculo"]
            .iter()
            .any(|w| n.contains(w))
        {
            out.push(Region::Musculoskeletal);
        }
        if n.contains("radiograph") && out.is_empty() {
            out.extend([Region::Cardiac, Region::Abdominal]);
        }
        out
    }

    /// Whether the set is offered in `region`: always for All, and for a
    /// set with no region.
    pub fn applies_to(&self, region: Region) -> bool {
        if region == Region::All {
            return true;
        }
        let r = self.regions();
        r.is_empty() || r.contains(&region)
    }
}

/// The kind of study a measurement belongs to, which decides the sets the
/// naming menus offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Region {
    Cardiac,
    Abdominal,
    Musculoskeletal,
    /// Every set.
    All,
}

impl Region {
    pub const ALL: [Region; 4] = [Region::Cardiac, Region::Abdominal, Region::Musculoskeletal, Region::All];

    pub fn label(self) -> &'static str {
        match self {
            Region::Cardiac => "Cardiac",
            Region::Abdominal => "Abdominal",
            Region::Musculoskeletal => "Musculoskeletal",
            Region::All => "All sets",
        }
    }
}

/// The kind of study, from its modality and what its descriptions and
/// body part say. Ultrasound with nothing else to go on is cardiac (an
/// echo); a radiograph or CT with nothing to go on offers every set.
pub fn infer_region(modality: &str, texts: &[&str]) -> Region {
    let t = texts.join(" ").to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| t.contains(w));
    if has(&["echo", "cardiac", "heart", "thorax", "chest", "lv ", "mitral", "aort"]) {
        return Region::Cardiac;
    }
    if has(&[
        "abdom", "kidney", "renal", "liver", "spleen", "bladder", "bowel", "intestin", "gastr", "adrenal", "uter",
        "prostat",
    ]) {
        return Region::Abdominal;
    }
    if has(&[
        "tendon", "ligament", "limb", "fetlock", "carpus", "tarsus", "hock", "stifle", "elbow", "shoulder", "hip",
        "pelvis", "joint", "digit", "hoof", "foot", "metacarp", "metatars", "spine", "msk", "ortho", "knee", "pastern",
    ]) {
        return Region::Musculoskeletal;
    }
    match modality.to_ascii_uppercase().as_str() {
        "US" => Region::Cardiac,
        _ => Region::All,
    }
}

/// A number derived from definitions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Calculation {
    pub key: String,
    pub label: String,
    /// E.g. `(lv.lvidd - lv.lvids) / lv.lvidd * 100`.
    pub expression: String,
    /// Unit lengths are converted to before evaluation: `mm` or `cm`.
    /// Areas follow (`mm2` or `cm2`), angles stay in degrees.
    pub input_unit: String,
    /// Unit of the result, shown after the number: `%`, `ml`, empty.
    pub result_unit: String,
    pub decimals: u8,
    /// Where the calculation applies (species or study type), informational.
    pub scope: Option<String>,
}

impl Default for Calculation {
    fn default() -> Self {
        Self {
            key: String::new(),
            label: String::new(),
            expression: String::new(),
            input_unit: "cm".into(),
            result_unit: String::new(),
            decimals: 1,
            scope: None,
        }
    }
}

/// Definitions, sets and calculations together; the shape of an exported
/// JSON file and of the user's part in settings.json.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Library {
    pub definitions: Vec<Definition>,
    pub sets: Vec<Set>,
    pub calculations: Vec<Calculation>,
}

impl Library {
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty() && self.sets.is_empty() && self.calculations.is_empty()
    }

    pub fn definition(&self, key: &str) -> Option<&Definition> {
        self.definitions.iter().find(|d| d.key == key)
    }

    pub fn calculation(&self, key: &str) -> Option<&Calculation> {
        self.calculations.iter().find(|c| c.key == key)
    }

    pub fn set(&self, name: &str) -> Option<&Set> {
        self.sets.iter().find(|s| s.name == name)
    }

    /// Label of a definition key, or the key itself.
    pub fn label(&self, key: &str) -> String {
        self.definition(key)
            .map(|d| d.label.clone())
            .unwrap_or_else(|| key.to_string())
    }

    /// The definition whose label matches, exactly or ignoring case.
    pub fn definition_by_label(&self, label: &str) -> Option<&Definition> {
        self.definitions
            .iter()
            .find(|d| d.label == label)
            .or_else(|| self.definitions.iter().find(|d| d.label.eq_ignore_ascii_case(label)))
    }

    /// Merge `other` into this library: entries with the same key or set
    /// name replace, new ones append. Returns the keys and names replaced.
    pub fn merge(&mut self, other: Library) -> MergeReport {
        let mut report = MergeReport::default();
        for d in other.definitions {
            match self.definitions.iter_mut().find(|x| x.key == d.key) {
                Some(x) => {
                    if *x != d {
                        report.replaced_definitions.push(d.key.clone());
                    }
                    *x = d;
                }
                None => {
                    report.added += 1;
                    self.definitions.push(d);
                }
            }
        }
        for s in other.sets {
            match self.sets.iter_mut().find(|x| x.name == s.name) {
                Some(x) => {
                    if *x != s {
                        report.replaced_sets.push(s.name.clone());
                    }
                    *x = s;
                }
                None => {
                    report.added += 1;
                    self.sets.push(s);
                }
            }
        }
        for c in other.calculations {
            match self.calculations.iter_mut().find(|x| x.key == c.key) {
                Some(x) => {
                    if *x != c {
                        report.replaced_calculations.push(c.key.clone());
                    }
                    *x = c;
                }
                None => {
                    report.added += 1;
                    self.calculations.push(c);
                }
            }
        }
        report
    }

    /// The library as pretty JSON, the export file format.
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".into())
    }

    /// Read a library from JSON text.
    pub fn from_json(text: &str) -> Result<Library, String> {
        serde_json::from_str(text).map_err(|e| e.to_string())
    }

    /// What an import would replace, without changing anything.
    pub fn preview_merge(&self, other: &Library) -> MergeReport {
        let mut copy = self.clone();
        copy.merge(other.clone())
    }
}

/// What a merge changed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MergeReport {
    pub added: usize,
    pub replaced_definitions: Vec<String>,
    pub replaced_sets: Vec<String>,
    pub replaced_calculations: Vec<String>,
}

impl MergeReport {
    pub fn replaces_anything(&self) -> bool {
        !(self.replaced_definitions.is_empty()
            && self.replaced_sets.is_empty()
            && self.replaced_calculations.is_empty())
    }
}

// ------------------------------------------------------------- builtins

/// The library Ruthos ships. Keys are stable; labels follow veterinary
/// usage (the LV posterior wall is the free wall).
pub fn builtin() -> Library {
    let def = |key: &str, label: &str, tool: &str, quantity: &str, unit: &str, decimals: u8, concept: Option<&str>| {
        Definition {
            key: key.into(),
            label: label.into(),
            tool: tool.into(),
            quantity: quantity.into(),
            unit: unit.into(),
            decimals,
            concept: concept.map(String::from),
            species: None,
        }
    };
    let calc = |key: &str,
                label: &str,
                expression: &str,
                input_unit: &str,
                result_unit: &str,
                decimals: u8,
                scope: Option<&str>| Calculation {
        key: key.into(),
        label: label.into(),
        expression: expression.into(),
        input_unit: input_unit.into(),
        result_unit: result_unit.into(),
        decimals,
        scope: scope.map(String::from),
    };
    #[allow(unused_variables)]
    let set = |name: &str, keys: &[&str]| Set {
        name: name.into(),
        keys: keys.iter().map(|k| k.to_string()).collect(),
        regions: Vec::new(),
    };
    let set_in = |name: &str, regions: &[Region], keys: &[&str]| Set {
        name: name.into(),
        keys: keys.iter().map(|k| k.to_string()).collect(),
        regions: regions.to_vec(),
    };
    Library {
        definitions: vec![
            // Echo: LV M-mode
            def("lv.ivsd", "IVSd", "lv-mmode", "length", "mm", 1, Some("LN:18154-5")),
            def("lv.lvidd", "LVIDd", "lv-mmode", "length", "mm", 1, Some("LN:29436-3")),
            def("lv.lvfwd", "LVFWd", "lv-mmode", "length", "mm", 1, Some("LN:18152-9")),
            def("lv.ivss", "IVSs", "lv-mmode", "length", "mm", 1, Some("LN:18153-7")),
            def("lv.lvids", "LVIDs", "lv-mmode", "length", "mm", 1, Some("LN:29438-9")),
            def("lv.lvfws", "LVFWs", "lv-mmode", "length", "mm", 1, Some("LN:18151-1")),
            // Echo: 2D
            def("echo.ao", "Ao", "caliper", "length", "mm", 1, Some("LN:18014-1")),
            def("echo.la", "LA", "caliper", "length", "mm", 1, Some("LN:29469-4")),
            def("echo.rvidd", "RVIDd", "caliper", "length", "mm", 1, None),
            def("echo.epss", "EPSS", "caliper", "length", "mm", 1, None),
            def("echo.pa", "PA", "caliper", "length", "mm", 1, None),
            // Echo: spectral Doppler (velocities in m/s, times in ms)
            def("echo.mv_e", "MV E", "point", "velocity", "m/s", 2, None),
            def("echo.mv_a", "MV A", "point", "velocity", "m/s", 2, None),
            def("echo.mv_edt", "E deceleration time", "caliper", "time", "ms", 0, None),
            def("echo.mv_dec_slope", "E deceleration slope", "caliper", "slope", "m/s2", 2, None),
            def("echo.mv_pht", "MV pressure half-time", "caliper", "pht", "ms", 0, None),
            def("echo.ao_vmax", "Ao Vmax", "point", "velocity", "m/s", 2, None),
            def("echo.pa_vmax", "PA Vmax", "point", "velocity", "m/s", 2, None),
            def("echo.mr_vmax", "MR Vmax", "point", "velocity", "m/s", 2, None),
            def("echo.tr_vmax", "TR Vmax", "point", "velocity", "m/s", 2, None),
            def("echo.ar_vmax", "AR Vmax", "point", "velocity", "m/s", 2, None),
            def("echo.ar_pht", "AR pressure half-time", "caliper", "pht", "ms", 0, None),
            // Timing
            def("ecg.rr", "RR interval", "caliper", "time", "ms", 0, None),
            // Radiography
            def("vhs.long", "VHS long axis", "caliper", "length", "mm", 1, None),
            def("vhs.short", "VHS short axis", "caliper", "length", "mm", 1, None),
            def("vhs.t4", "T4 vertebral length", "caliper", "length", "mm", 1, None),
            def("kidney.left", "Kidney length (L)", "caliper", "length", "mm", 1, None),
            def("kidney.right", "Kidney length (R)", "caliper", "length", "mm", 1, None),
            // Orthopaedic
            def("ortho.norberg", "Norberg angle", "angle", "angle", "deg", 1, None),
            def("ortho.tpa", "Tibial plateau angle", "angle", "angle", "deg", 1, None),
            def("ortho.patellar_ligament", "Patellar ligament length", "caliper", "length", "mm", 1, None),
            def("ortho.patella", "Patella length", "caliper", "length", "mm", 1, None),
            // Abdomen (ultrasound)
            def("abd.adrenal_left", "Adrenal thickness (L)", "caliper", "length", "mm", 1, None),
            def("abd.adrenal_right", "Adrenal thickness (R)", "caliper", "length", "mm", 1, None),
            def("abd.bladder_wall", "Bladder wall", "caliper", "length", "mm", 1, None),
            def("abd.gastric_wall", "Gastric wall", "caliper", "length", "mm", 1, None),
            def("abd.si_wall", "Small intestinal wall", "caliper", "length", "mm", 1, None),
            def("abd.spleen", "Spleen thickness", "caliper", "length", "mm", 1, None),
            def("abd.ln", "Lymph node", "caliper", "length", "mm", 1, None),
            // Tendons and ligaments (equine), cross-sectional areas
            def("tendon.sdft", "SDFT CSA", "area", "area", "mm2", 0, None),
            def("tendon.ddft", "DDFT CSA", "area", "area", "mm2", 0, None),
            def("tendon.icl", "ICL (accessory ligament of the DDFT) CSA", "area", "area", "mm2", 0, None),
            def("tendon.sl", "Suspensory ligament CSA", "area", "area", "mm2", 0, None),
            def("tendon.sdft_thickness", "SDFT thickness", "caliper", "length", "mm", 1, None),
        ],
        sets: vec![
            set_in(
                "Echo: LV M-mode",
                &[Region::Cardiac],
                &["lv.ivsd", "lv.lvidd", "lv.lvfwd", "lv.ivss", "lv.lvids", "lv.lvfws", "ecg.rr"],
            ),
            set_in("Echo: 2D", &[Region::Cardiac], &["echo.ao", "echo.la", "echo.rvidd", "echo.epss", "echo.pa"]),
            set_in(
                "Echo: Doppler",
                &[Region::Cardiac],
                &[
                    "echo.mv_e",
                    "echo.mv_a",
                    "echo.mv_edt",
                    "echo.mv_dec_slope",
                    "echo.mv_pht",
                    "echo.ao_vmax",
                    "echo.pa_vmax",
                    "echo.mr_vmax",
                    "echo.tr_vmax",
                    "echo.ar_vmax",
                    "echo.ar_pht",
                    "ecg.rr",
                ],
            ),
            set_in(
                "Radiography",
                &[Region::Cardiac, Region::Abdominal],
                &["vhs.long", "vhs.short", "vhs.t4", "kidney.left", "kidney.right"],
            ),
            set_in(
                "Orthopaedic",
                &[Region::Musculoskeletal],
                &["ortho.norberg", "ortho.tpa", "ortho.patellar_ligament", "ortho.patella"],
            ),
            set_in(
                "Abdomen",
                &[Region::Abdominal],
                &[
                    "kidney.left",
                    "kidney.right",
                    "abd.adrenal_left",
                    "abd.adrenal_right",
                    "abd.bladder_wall",
                    "abd.gastric_wall",
                    "abd.si_wall",
                    "abd.spleen",
                    "abd.ln",
                ],
            ),
            set_in(
                "Tendons (equine)",
                &[Region::Musculoskeletal],
                &["tendon.sdft", "tendon.ddft", "tendon.icl", "tendon.sl", "tendon.sdft_thickness"],
            ),
        ],
        calculations: vec![
            calc("lv.fs", "Fractional shortening", "(lv.lvidd - lv.lvids) / lv.lvidd * 100", "cm", "%", 1, Some("echo")),
            calc("lv.edv_teichholz", "EDV (Teichholz)", "7 / (2.4 + lv.lvidd) * lv.lvidd^3", "cm", "ml", 1, Some("echo")),
            calc("lv.esv_teichholz", "ESV (Teichholz)", "7 / (2.4 + lv.lvids) * lv.lvids^3", "cm", "ml", 1, Some("echo")),
            calc(
                "lv.ef_teichholz",
                "EF (Teichholz)",
                "((7 / (2.4 + lv.lvidd) * lv.lvidd^3) - (7 / (2.4 + lv.lvids) * lv.lvids^3)) / (7 / (2.4 + lv.lvidd) * lv.lvidd^3) * 100",
                "cm",
                "%",
                1,
                Some("echo"),
            ),
            calc("echo.la_ao", "LA/Ao", "echo.la / echo.ao", "cm", "", 2, Some("echo")),
            calc("echo.mv_ea", "E/A", "echo.mv_e / echo.mv_a", "cm", "", 2, Some("echo")),
            calc("ecg.hr", "Heart rate", "60000 / ecg.rr", "cm", "bpm", 0, None),
            calc("echo.mva_pht", "MVA (pressure half-time)", "220 / echo.mv_pht", "cm", "cm\u{b2}", 2, Some("echo")),
            calc("echo.tr_pg", "TR peak gradient", "4 * echo.tr_vmax^2", "cm", "mmHg", 1, Some("echo")),
            calc("echo.mr_pg", "MR peak gradient", "4 * echo.mr_vmax^2", "cm", "mmHg", 1, Some("echo")),
            calc("echo.ao_pg", "Ao peak gradient", "4 * echo.ao_vmax^2", "cm", "mmHg", 1, Some("echo")),
            calc("echo.pa_pg", "PA peak gradient", "4 * echo.pa_vmax^2", "cm", "mmHg", 1, Some("echo")),
            calc("vhs.total", "Vertebral heart score", "(vhs.long + vhs.short) / vhs.t4", "cm", "v", 1, Some("thorax")),
            calc("ortho.patellar_ratio", "Patellar ligament : patella", "ortho.patellar_ligament / ortho.patella", "cm", "", 2, Some("stifle")),
        ],
    }
}

/// The built-in library with the user's entries laid over it: a user entry
/// with a built-in key replaces the built-in one, others are appended.
pub fn effective(user: &Library) -> Library {
    let mut lib = builtin();
    lib.merge(user.clone());
    lib
}

/// Whether a key or set name belongs to the shipped library.
pub fn is_builtin_definition(key: &str) -> bool {
    builtin().definition(key).is_some()
}

pub fn is_builtin_calculation(key: &str) -> bool {
    builtin().calculation(key).is_some()
}

pub fn is_builtin_set(name: &str) -> bool {
    builtin().set(name).is_some()
}

// ----------------------------------------------------------- validation

/// Keys are lower-case letters, digits, dots and underscores, starting
/// with a letter.
pub fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_')
        && !key.ends_with('.')
        && !key.contains("..")
}

/// Problems in a library, one line each; empty when it is sound.
pub fn validate(lib: &Library) -> Vec<String> {
    let mut problems = Vec::new();
    let mut seen = HashSet::new();
    for d in &lib.definitions {
        if !valid_key(&d.key) {
            problems.push(format!(
                "Definition key '{}' is not lower-case dotted (like lv.lvidd)",
                d.key
            ));
        }
        if !seen.insert(d.key.clone()) {
            problems.push(format!("Definition key '{}' appears twice", d.key));
        }
        if d.label.trim().is_empty() {
            problems.push(format!("Definition '{}' has no label", d.key));
        }
        if !["caliper", "length", "angle", "area", "point", "lv-mmode"].contains(&d.tool.as_str()) {
            problems.push(format!(
                "Definition '{}': tool '{}' is not caliper, angle, area, point or lv-mmode",
                d.key, d.tool
            ));
        }
    }
    let mut seen_calc = HashSet::new();
    for c in &lib.calculations {
        if !valid_key(&c.key) {
            problems.push(format!("Calculation key '{}' is not lower-case dotted", c.key));
        }
        if !seen_calc.insert(c.key.clone()) {
            problems.push(format!("Calculation key '{}' appears twice", c.key));
        }
        if seen.contains(&c.key) {
            problems.push(format!("'{}' is both a definition and a calculation", c.key));
        }
        match Expr::parse(&c.expression) {
            Ok(e) => {
                for k in e.keys() {
                    if lib.definition(&k).is_none() {
                        problems.push(format!("Calculation '{}' uses unknown key '{k}'", c.key));
                    }
                }
            }
            Err(e) => problems.push(format!("Calculation '{}': {e}", c.key)),
        }
        if !["mm", "cm"].contains(&c.input_unit.as_str()) {
            problems.push(format!("Calculation '{}': input unit must be mm or cm", c.key));
        }
    }
    let mut names = HashSet::new();
    for s in &lib.sets {
        if s.name.trim().is_empty() {
            problems.push("A set has no name".into());
        }
        if !names.insert(s.name.clone()) {
            problems.push(format!("Set '{}' appears twice", s.name));
        }
        for k in &s.keys {
            if lib.definition(k).is_none() {
                problems.push(format!("Set '{}' lists unknown key '{k}'", s.name));
            }
        }
    }
    problems
}

/// Describe one calculation for a live editor: the keys it uses and
/// whether it is scale independent, or the parse error.
pub fn describe_calculation(lib: &Library, c: &Calculation) -> Result<String, String> {
    let expr = Expr::parse(&c.expression).map_err(|e| e.to_string())?;
    let keys = expr.keys();
    let unknown: Vec<String> = keys.iter().filter(|k| lib.definition(k).is_none()).cloned().collect();
    if !unknown.is_empty() {
        return Err(format!(
            "unknown key{} {}",
            if unknown.len() == 1 { "" } else { "s" },
            unknown.join(", ")
        ));
    }
    let mut text = if keys.is_empty() {
        "uses no measurement".to_string()
    } else {
        format!("uses {}", keys.join(", "))
    };
    if is_scale_independent(lib, c) {
        text.push_str("; scale independent (unaffected by magnification)");
    } else {
        text.push_str("; depends on the scale of its inputs");
    }
    Ok(text)
}

// ----------------------------------------------------------- expressions

/// A parsed expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Num(f64),
    Key(String),
    Neg(Box<Expr>),
    Bin(Box<Expr>, Op, Box<Expr>),
    Call(Func, Vec<Expr>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Func {
    Sqrt,
    Abs,
    Min,
    Max,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EvalError {
    Missing(String),
    DivideByZero,
    NotFinite,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EvalError::Missing(k) => write!(f, "no value for {k}"),
            EvalError::DivideByZero => write!(f, "division by zero"),
            EvalError::NotFinite => write!(f, "result is not a number"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Num(f64),
    Ident(String),
    Op(char),
    LParen,
    RParen,
    Comma,
}

fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let mut out = Vec::new();
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '0'..='9' | '.' if c != '.' || chars.get(i + 1).is_some_and(|d| d.is_ascii_digit()) => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                out.push(Token::Num(text.parse().map_err(|_| format!("bad number '{text}'"))?));
            }
            'a'..='z' | 'A'..='Z' | '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '.' || chars[i] == '_') {
                    i += 1;
                }
                let text: String = chars[start..i].iter().collect();
                out.push(Token::Ident(text));
            }
            '+' | '-' | '*' | '/' | '^' => {
                out.push(Token::Op(c));
                i += 1;
            }
            '\u{d7}' => {
                out.push(Token::Op('*'));
                i += 1;
            }
            '\u{f7}' => {
                out.push(Token::Op('/'));
                i += 1;
            }
            '\u{2212}' => {
                out.push(Token::Op('-'));
                i += 1;
            }
            '(' => {
                out.push(Token::LParen);
                i += 1;
            }
            ')' => {
                out.push(Token::RParen);
                i += 1;
            }
            ',' => {
                out.push(Token::Comma);
                i += 1;
            }
            other => return Err(format!("unexpected character '{other}'")),
        }
    }
    Ok(out)
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        t
    }

    fn expect(&mut self, t: Token, what: &str) -> Result<(), String> {
        match self.next() {
            Some(got) if got == t => Ok(()),
            Some(got) => Err(format!("expected {what}, found {}", describe(&got))),
            None => Err(format!("expected {what} at the end")),
        }
    }

    // sum := product (('+' | '-') product)*
    fn sum(&mut self) -> Result<Expr, String> {
        let mut left = self.product()?;
        while let Some(Token::Op(c @ ('+' | '-'))) = self.peek().cloned() {
            self.next();
            let right = self.product()?;
            left = Expr::Bin(
                Box::new(left),
                if c == '+' { Op::Add } else { Op::Sub },
                Box::new(right),
            );
        }
        Ok(left)
    }

    // product := power (('*' | '/') power)*
    fn product(&mut self) -> Result<Expr, String> {
        let mut left = self.power()?;
        while let Some(Token::Op(c @ ('*' | '/'))) = self.peek().cloned() {
            self.next();
            let right = self.power()?;
            left = Expr::Bin(
                Box::new(left),
                if c == '*' { Op::Mul } else { Op::Div },
                Box::new(right),
            );
        }
        Ok(left)
    }

    // power := unary ('^' power)?   (right associative)
    fn power(&mut self) -> Result<Expr, String> {
        let base = self.unary()?;
        if let Some(Token::Op('^')) = self.peek() {
            self.next();
            let exp = self.power()?;
            return Ok(Expr::Bin(Box::new(base), Op::Pow, Box::new(exp)));
        }
        Ok(base)
    }

    // unary := '-' unary | atom
    fn unary(&mut self) -> Result<Expr, String> {
        if let Some(Token::Op('-')) = self.peek() {
            self.next();
            return Ok(Expr::Neg(Box::new(self.unary()?)));
        }
        if let Some(Token::Op('+')) = self.peek() {
            self.next();
            return self.unary();
        }
        self.atom()
    }

    // atom := number | key | func '(' args ')' | '(' sum ')'
    fn atom(&mut self) -> Result<Expr, String> {
        match self.next() {
            Some(Token::Num(n)) => Ok(Expr::Num(n)),
            Some(Token::Ident(name)) => {
                if let Some(Token::LParen) = self.peek() {
                    let func = match name.as_str() {
                        "sqrt" => Func::Sqrt,
                        "abs" => Func::Abs,
                        "min" => Func::Min,
                        "max" => Func::Max,
                        other => return Err(format!("unknown function '{other}' (sqrt, abs, min, max)")),
                    };
                    self.next();
                    let mut args = Vec::new();
                    if let Some(Token::RParen) = self.peek() {
                        return Err(format!("{} needs an argument", name));
                    }
                    loop {
                        args.push(self.sum()?);
                        match self.next() {
                            Some(Token::Comma) => continue,
                            Some(Token::RParen) => break,
                            Some(t) => return Err(format!("expected ',' or ')', found {}", describe(&t))),
                            None => return Err("missing ')'".into()),
                        }
                    }
                    let wanted = match func {
                        Func::Sqrt | Func::Abs => 1,
                        Func::Min | Func::Max => 2,
                    };
                    if args.len() != wanted {
                        return Err(format!(
                            "{name} takes {wanted} argument{}",
                            if wanted == 1 { "" } else { "s" }
                        ));
                    }
                    Ok(Expr::Call(func, args))
                } else {
                    Ok(Expr::Key(name))
                }
            }
            Some(Token::LParen) => {
                let inner = self.sum()?;
                self.expect(Token::RParen, "')'")?;
                Ok(inner)
            }
            Some(t) => Err(format!("unexpected {}", describe(&t))),
            None => Err("the expression ends early".into()),
        }
    }
}

fn describe(t: &Token) -> String {
    match t {
        Token::Num(n) => format!("number {n}"),
        Token::Ident(s) => format!("'{s}'"),
        Token::Op(c) => format!("'{c}'"),
        Token::LParen => "'('".into(),
        Token::RParen => "')'".into(),
        Token::Comma => "','".into(),
    }
}

impl Expr {
    pub fn parse(src: &str) -> Result<Expr, String> {
        if src.trim().is_empty() {
            return Err("the expression is empty".into());
        }
        let tokens = tokenize(src)?;
        let mut p = Parser { tokens, pos: 0 };
        let e = p.sum()?;
        if let Some(t) = p.peek() {
            return Err(format!("unexpected {} after the expression", describe(t)));
        }
        Ok(e)
    }

    /// Definition keys the expression refers to, in first-use order.
    pub fn keys(&self) -> Vec<String> {
        let mut out = Vec::new();
        self.collect_keys(&mut out);
        out
    }

    fn collect_keys(&self, out: &mut Vec<String>) {
        match self {
            Expr::Num(_) => {}
            Expr::Key(k) => {
                if !out.contains(k) {
                    out.push(k.clone());
                }
            }
            Expr::Neg(e) => e.collect_keys(out),
            Expr::Bin(a, _, b) => {
                a.collect_keys(out);
                b.collect_keys(out);
            }
            Expr::Call(_, args) => args.iter().for_each(|a| a.collect_keys(out)),
        }
    }

    pub fn eval(&self, lookup: &dyn Fn(&str) -> Option<f64>) -> Result<f64, EvalError> {
        let v = match self {
            Expr::Num(n) => *n,
            Expr::Key(k) => lookup(k).ok_or_else(|| EvalError::Missing(k.clone()))?,
            Expr::Neg(e) => -e.eval(lookup)?,
            Expr::Bin(a, op, b) => {
                let (x, y) = (a.eval(lookup)?, b.eval(lookup)?);
                match op {
                    Op::Add => x + y,
                    Op::Sub => x - y,
                    Op::Mul => x * y,
                    Op::Div => {
                        if y == 0.0 {
                            return Err(EvalError::DivideByZero);
                        }
                        x / y
                    }
                    Op::Pow => x.powf(y),
                }
            }
            Expr::Call(f, args) => {
                let v: Vec<f64> = args.iter().map(|a| a.eval(lookup)).collect::<Result<_, _>>()?;
                match f {
                    Func::Sqrt => v[0].sqrt(),
                    Func::Abs => v[0].abs(),
                    Func::Min => v[0].min(v[1]),
                    Func::Max => v[0].max(v[1]),
                }
            }
        };
        if v.is_finite() {
            Ok(v)
        } else {
            Err(EvalError::NotFinite)
        }
    }
}

// ----------------------------------------------------------- evaluation

/// A measured input: the stored value in the definition's unit, and
/// whether its scale is the patient's (false for detector-plane and
/// uncalibrated values).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Input {
    pub value: f64,
    pub patient_scale: bool,
}

/// Convert a stored value to the calculation's input unit.
fn convert(def: &Definition, value: f64, input_unit: &str) -> f64 {
    match (def.quantity.as_str(), def.unit.as_str(), input_unit) {
        ("length", "mm", "cm") => value / 10.0,
        ("length", "cm", "mm") => value * 10.0,
        ("area", "mm2", "cm") => value / 100.0,
        ("area", "cm2", "mm") => value * 100.0,
        _ => value,
    }
}

/// The outcome of one calculation over a study's values.
#[derive(Debug, Clone, PartialEq)]
pub struct CalcResult {
    pub key: String,
    pub label: String,
    pub value: f64,
    pub unit: String,
    /// `value` rounded to the calculation's decimals, with its unit.
    pub text: String,
    pub scale_independent: bool,
    /// True when every input was measured at the patient's scale, or the
    /// calculation does not depend on scale.
    pub trustworthy_scale: bool,
    pub inputs: Vec<String>,
}

/// Evaluate a calculation against values keyed by definition.
pub fn evaluate(lib: &Library, c: &Calculation, values: &BTreeMap<String, Input>) -> Result<CalcResult, EvalError> {
    let expr = Expr::parse(&c.expression).map_err(|_| EvalError::Missing(c.key.clone()))?;
    let lookup = |k: &str| -> Option<f64> {
        let def = lib.definition(k)?;
        values.get(k).map(|v| convert(def, v.value, &c.input_unit))
    };
    let value = expr.eval(&lookup)?;
    let keys = expr.keys();
    let scale_independent = is_scale_independent(lib, c);
    let all_patient = keys.iter().all(|k| values.get(k).is_some_and(|v| v.patient_scale));
    let text = format!(
        "{:.*}{}{}",
        c.decimals as usize,
        value,
        if c.result_unit.is_empty() { "" } else { " " },
        c.result_unit
    );
    Ok(CalcResult {
        key: c.key.clone(),
        label: c.label.clone(),
        value,
        unit: c.result_unit.clone(),
        text,
        scale_independent,
        trustworthy_scale: scale_independent || all_patient,
        inputs: keys,
    })
}

/// Evaluate every calculation whose inputs are all present.
/// Every calculation a study's measurements allow, each input the mean of its
/// values, with how many values each mean holds. Pixel values are left out, and
/// fractional shortening is left to the M-mode block when the study has one.
pub fn study_calculations(
    lib: &Library,
    inputs: &[crate::model::MeasurementInput],
    has_mmode: bool,
) -> (Vec<CalcResult>, BTreeMap<String, usize>) {
    // Sum, count and scale of each definition's values.
    let mut acc: BTreeMap<String, (f64, usize, bool)> = BTreeMap::new();
    for m in inputs.iter().filter(|m| m.unit != "px") {
        let patient_scale = m
            .basis
            .as_deref()
            .and_then(ruthos_dicom::SpacingBasis::from_key)
            .map(|b| b.is_patient_scale())
            .unwrap_or(true);
        let e = acc.entry(m.def_key.clone()).or_insert((0.0, 0, true));
        e.0 += m.value;
        e.1 += 1;
        e.2 &= patient_scale;
    }
    let counts = acc.iter().map(|(k, v)| (k.clone(), v.1)).collect();
    let values: BTreeMap<String, Input> = acc
        .into_iter()
        .map(|(k, (sum, n, patient_scale))| {
            (
                k,
                Input {
                    value: sum / n as f64,
                    patient_scale,
                },
            )
        })
        .collect();
    let results = evaluate_all(lib, &values)
        .into_iter()
        .filter(|r| !(has_mmode && r.key == "lv.fs"))
        .collect();
    (results, counts)
}

pub fn evaluate_all(lib: &Library, values: &BTreeMap<String, Input>) -> Vec<CalcResult> {
    lib.calculations
        .iter()
        .filter_map(|c| evaluate(lib, c, values).ok())
        .collect()
}

/// Whether the result is unchanged when every length and area input is
/// magnified together: true for ratios such as VHS, false for a volume.
/// Decided numerically: the expression is evaluated with a sample of
/// inputs and again with lengths doubled and areas quadrupled.
pub fn is_scale_independent(lib: &Library, c: &Calculation) -> bool {
    let Ok(expr) = Expr::parse(&c.expression) else {
        return false;
    };
    let keys = expr.keys();
    if keys.is_empty() {
        return true;
    }
    let scaled_quantity = |k: &str| {
        lib.definition(k)
            .map(|d| d.quantity.clone())
            .unwrap_or_else(|| "length".into())
    };
    if keys
        .iter()
        .all(|k| !matches!(scaled_quantity(k).as_str(), "length" | "area"))
    {
        return true;
    }
    // Several sample points, so a coincidence at one does not mislead.
    let samples: [f64; 3] = [1.0, 1.7, 3.3];
    for (i, base) in samples.iter().enumerate() {
        let value_of = |k: &str, factor: f64| -> Option<f64> {
            let idx = keys.iter().position(|x| x == k)? as f64;
            let v = base * (1.0 + 0.37 * (idx + 1.0) + 0.11 * i as f64);
            Some(match scaled_quantity(k).as_str() {
                "length" => v * factor,
                "area" => v * factor * factor,
                _ => v,
            })
        };
        let a = expr.eval(&|k| value_of(k, 1.0));
        let b = expr.eval(&|k| value_of(k, 2.0));
        match (a, b) {
            (Ok(a), Ok(b)) => {
                let tol = 1e-9 * a.abs().max(1.0);
                if (a - b).abs() > tol {
                    return false;
                }
            }
            _ => return false,
        }
    }
    true
}

// ------------------------------------------------------------ migration

/// Turn the old `measurement_sets` (names and labels) into a user library:
/// labels that match a built-in definition map to its key, others become
/// new definitions keyed from their label.
pub fn migrate_label_sets(sets: &[(String, Vec<String>)]) -> Library {
    let builtin = builtin();
    let mut lib = Library::default();
    for (name, labels) in sets {
        let mut keys = Vec::new();
        for label in labels {
            let label = label.trim();
            if label.is_empty() {
                continue;
            }
            let key = match builtin.definition_by_label(label) {
                Some(d) => d.key.clone(),
                None => {
                    let key = slug_key(label);
                    if lib.definition(&key).is_none() {
                        lib.definitions.push(Definition {
                            key: key.clone(),
                            label: label.to_string(),
                            ..Default::default()
                        });
                    }
                    key
                }
            };
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
        // A set identical to the built-in one need not be stored.
        if builtin.set(name).is_some_and(|s| s.keys == keys) {
            continue;
        }
        lib.sets.push(Set {
            name: name.clone(),
            keys,
            ..Default::default()
        });
    }
    lib
}

/// A key made from a label: "Kidney length (L)" becomes `custom.kidney_length_l`.
pub fn slug_key(label: &str) -> String {
    let mut s = String::from("custom.");
    let mut last_us = false;
    for c in label.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            s.push(c);
            last_us = false;
        } else if !last_us && s.len() > 7 {
            s.push('_');
            last_us = true;
        }
    }
    while s.ends_with('_') {
        s.pop();
    }
    if s.len() == 7 {
        s.push_str("measurement");
    }
    s
}

/// Label-to-key pairs for giving stored measurements their definition key.
pub fn label_keys(lib: &Library) -> Vec<(String, String)> {
    lib.definitions
        .iter()
        .map(|d| (d.label.clone(), d.key.clone()))
        .collect()
}

#[cfg(test)]
mod region_tests {
    use super::*;

    #[test]
    fn sets_belong_to_regions() {
        let lib = builtin();
        let names = |r: Region| -> Vec<String> {
            lib.sets
                .iter()
                .filter(|s| s.applies_to(r))
                .map(|s| s.name.clone())
                .collect()
        };
        assert!(names(Region::Cardiac)
            .iter()
            .all(|n| n.starts_with("Echo") || n == "Radiography"));
        assert_eq!(names(Region::Musculoskeletal), ["Orthopaedic", "Tendons (equine)"]);
        assert!(names(Region::Abdominal).contains(&"Abdomen".to_string()));
        assert_eq!(names(Region::All).len(), lib.sets.len());
        // A set saved before regions existed is placed by its name; one with
        // no clue is offered everywhere.
        let old = Set {
            name: "My echo extras".into(),
            ..Default::default()
        };
        assert_eq!(old.regions(), [Region::Cardiac]);
        let mine = Set {
            name: "Mine".into(),
            ..Default::default()
        };
        assert!(mine.applies_to(Region::Abdominal) && mine.applies_to(Region::Cardiac));
    }

    #[test]
    fn studies_are_placed_by_what_they_say() {
        assert_eq!(infer_region("US", &["Echo", ""]), Region::Cardiac);
        assert_eq!(infer_region("US", &["ABDOMEN", "Left kidney"]), Region::Abdominal);
        assert_eq!(infer_region("US", &["L fore fetlock", ""]), Region::Musculoskeletal);
        assert_eq!(infer_region("US", &["", ""]), Region::Cardiac);
        assert_eq!(infer_region("DX", &["Thorax LAT"]), Region::Cardiac);
        assert_eq!(infer_region("DX", &["Stifle"]), Region::Musculoskeletal);
        assert_eq!(infer_region("CT", &[""]), Region::All);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, f64)]) -> BTreeMap<String, Input> {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    k.to_string(),
                    Input {
                        value: *v,
                        patient_scale: true,
                    },
                )
            })
            .collect()
    }

    #[test]
    fn builtin_library_is_sound() {
        let lib = builtin();
        assert!(validate(&lib).is_empty(), "{:?}", validate(&lib));
        assert_eq!(lib.set("Echo: LV M-mode").unwrap().keys[2], "lv.lvfwd");
    }

    #[test]
    fn expressions_parse_and_evaluate() {
        let e = Expr::parse("(a.x - a.y) / a.x * 100").unwrap();
        assert_eq!(e.keys(), vec!["a.x", "a.y"]);
        let v = e.eval(&|k| match k {
            "a.x" => Some(50.0),
            "a.y" => Some(30.0),
            _ => None,
        });
        assert!((v.unwrap() - 40.0).abs() < 1e-9);
        assert!(
            (Expr::parse("2 ^ 3 ^ 2").unwrap().eval(&|_| None).unwrap() - 512.0).abs() < 1e-9,
            "powers are right associative"
        );
        assert!(
            (Expr::parse("sqrt(16) + abs(-2) + min(1, 2) + max(1, 2) - -1")
                .unwrap()
                .eval(&|_| None)
                .unwrap()
                - 10.0)
                .abs()
                < 1e-9
        );
        assert!(
            (Expr::parse("7 \u{f7} (2.4 + 4) \u{d7} 4^3")
                .unwrap()
                .eval(&|_| None)
                .unwrap()
                - 70.0)
                .abs()
                < 1e-9
        );
        assert!(Expr::parse("1 +").is_err());
        assert!(Expr::parse("foo(1)").is_err());
        assert!(Expr::parse("1 2").is_err());
        assert_eq!(
            Expr::parse("1 / x").unwrap().eval(&|_| Some(0.0)),
            Err(EvalError::DivideByZero)
        );
        assert_eq!(
            Expr::parse("x").unwrap().eval(&|_| None),
            Err(EvalError::Missing("x".into()))
        );
    }

    #[test]
    fn teichholz_converts_millimetres_to_centimetres() {
        let lib = builtin();
        let c = lib.calculation("lv.edv_teichholz").unwrap();
        // LVIDd 40 mm = 4 cm: 7 / 6.4 * 64 = 70 ml.
        let r = evaluate(&lib, c, &values(&[("lv.lvidd", 40.0)])).unwrap();
        assert!((r.value - 70.0).abs() < 1e-9);
        assert_eq!(r.text, "70.0 ml");
        assert!(!r.scale_independent);
        let fs = evaluate(
            &lib,
            lib.calculation("lv.fs").unwrap(),
            &values(&[("lv.lvidd", 50.0), ("lv.lvids", 30.0)]),
        )
        .unwrap();
        assert!((fs.value - 40.0).abs() < 1e-9);
        assert!(fs.scale_independent);
    }

    #[test]
    fn heart_rate_from_the_rr_interval() {
        let lib = builtin();
        let r = evaluate(&lib, lib.calculation("ecg.hr").unwrap(), &values(&[("ecg.rr", 750.0)])).unwrap();
        assert!((r.value - 80.0).abs() < 1e-9);
        assert_eq!(lib.definition("echo.ar_pht").unwrap().quantity, "pht");
        assert_eq!(lib.definition("echo.la").unwrap().tool, "caliper");
    }

    #[test]
    fn doppler_gradients_use_velocity_unchanged() {
        let lib = builtin();
        let r = evaluate(
            &lib,
            lib.calculation("echo.tr_pg").unwrap(),
            &values(&[("echo.tr_vmax", 3.0)]),
        )
        .unwrap();
        assert!((r.value - 36.0).abs() < 1e-9, "4 v squared with v in m/s");
        assert!(r.scale_independent, "velocities do not depend on the image scale");
    }

    #[test]
    fn scale_independence_is_detected() {
        let lib = builtin();
        assert!(is_scale_independent(&lib, lib.calculation("vhs.total").unwrap()));
        assert!(is_scale_independent(&lib, lib.calculation("echo.la_ao").unwrap()));
        assert!(!is_scale_independent(
            &lib,
            lib.calculation("lv.esv_teichholz").unwrap()
        ));
        let angle_only = Calculation {
            key: "t.a".into(),
            expression: "ortho.tpa * 2".into(),
            ..Default::default()
        };
        assert!(is_scale_independent(&lib, &angle_only));
    }

    #[test]
    fn detector_plane_inputs_flag_scale_dependent_results() {
        let lib = builtin();
        let mut v = values(&[("lv.lvidd", 40.0), ("lv.lvids", 30.0)]);
        v.get_mut("lv.lvidd").unwrap().patient_scale = false;
        let fs = evaluate(&lib, lib.calculation("lv.fs").unwrap(), &v).unwrap();
        assert!(fs.trustworthy_scale, "a ratio does not care about scale");
        let edv = evaluate(&lib, lib.calculation("lv.edv_teichholz").unwrap(), &v).unwrap();
        assert!(
            !edv.trustworthy_scale,
            "a volume from a detector-plane length is not the patient's"
        );
    }

    #[test]
    fn merge_replaces_by_key_and_reports() {
        let mut lib = builtin();
        let mut incoming = Library::default();
        incoming.definitions.push(Definition {
            key: "lv.lvidd".into(),
            label: "LVIDd (custom)".into(),
            ..Default::default()
        });
        incoming.definitions.push(Definition {
            key: "custom.new".into(),
            label: "New".into(),
            ..Default::default()
        });
        let preview = lib.preview_merge(&incoming);
        assert_eq!(preview.replaced_definitions, vec!["lv.lvidd"]);
        assert_eq!(preview.added, 1);
        assert_eq!(
            lib.definition("lv.lvidd").unwrap().label,
            "LVIDd",
            "preview changes nothing"
        );
        lib.merge(incoming);
        assert_eq!(lib.definition("lv.lvidd").unwrap().label, "LVIDd (custom)");
        assert!(lib.definition("custom.new").is_some());
    }

    #[test]
    fn user_entries_override_builtins_and_restore_by_removal() {
        let mut user = Library::default();
        user.definitions.push(Definition {
            key: "echo.la".into(),
            label: "Left atrium".into(),
            ..Default::default()
        });
        assert_eq!(effective(&user).definition("echo.la").unwrap().label, "Left atrium");
        user.definitions.clear();
        assert_eq!(effective(&user).definition("echo.la").unwrap().label, "LA");
    }

    #[test]
    fn validation_finds_problems() {
        let mut lib = builtin();
        lib.calculations.push(Calculation {
            key: "Bad Key".into(),
            expression: "lv.nope + 1".into(),
            ..Default::default()
        });
        lib.sets.push(Set {
            name: "S".into(),
            keys: vec!["lv.nope".into()],
            ..Default::default()
        });
        let problems = validate(&lib);
        assert!(problems.iter().any(|p| p.contains("not lower-case")));
        assert!(problems.iter().any(|p| p.contains("unknown key 'lv.nope'")));
        assert!(problems.iter().any(|p| p.contains("Set 'S'")));
        assert!(
            valid_key("lv.lvidd")
                && valid_key("custom.kidney_l")
                && !valid_key("LV")
                && !valid_key("a..b")
                && !valid_key("1a")
        );
    }

    #[test]
    fn old_label_sets_migrate_to_keys() {
        let old = vec![
            (
                "Echo: LV M-mode".to_string(),
                vec!["IVSd".to_string(), "LVIDd".into(), "LVPWd".into()],
            ),
            (
                "Mine".to_string(),
                vec!["LVIDd".to_string(), "Kidney length (L)".into(), "Spleen width".into()],
            ),
        ];
        let lib = migrate_label_sets(&old);
        // LVPWd is not a built-in label any more, so it becomes a custom definition.
        let echo = lib.set("Echo: LV M-mode").unwrap();
        assert_eq!(echo.keys, vec!["lv.ivsd", "lv.lvidd", "custom.lvpwd"]);
        let mine = lib.set("Mine").unwrap();
        assert_eq!(mine.keys, vec!["lv.lvidd", "kidney.left", "custom.spleen_width"]);
        assert_eq!(lib.definition("custom.spleen_width").unwrap().label, "Spleen width");
        assert_eq!(slug_key("Kidney length (L)"), "custom.kidney_length_l");
        // A set equal to the built-in one is dropped.
        let same = vec![(
            "Radiography".to_string(),
            builtin()
                .set("Radiography")
                .unwrap()
                .keys
                .iter()
                .map(|k| builtin().label(k))
                .collect(),
        )];
        assert!(migrate_label_sets(&same).sets.is_empty());
    }

    #[test]
    fn library_round_trips_through_json() {
        let lib = builtin();
        let text = serde_json::to_string_pretty(&lib).unwrap();
        let back: Library = serde_json::from_str(&text).unwrap();
        assert_eq!(back, lib);
    }
}
