//! Reading a DVC project from disk: `dvc.yaml`, `dvc.lock` and parameter files.
//!
//! Two layers of types: private `Raw*` structs mirror the file formats (including their
//! flexibility), and the public types are the tidy shapes the rest of the app uses.

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::Value;

pub const DVC_YAML: &str = "dvc.yaml";
pub const DVC_LOCK: &str = "dvc.lock";
/// DVC reads params from this file when a `params:` entry names no file.
pub const DEFAULT_PARAMS_FILE: &str = "params.yaml";

// ---------------------------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------------------------

/// A loaded DVC project.
#[derive(Debug)]
pub struct Project {
    pub root: PathBuf,
    /// Stages in the order they are declared in `dvc.yaml`.
    pub stages: Vec<Stage>,
    pub lock: Lock,
}

/// One stage from `dvc.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stage {
    pub name: String,
    /// A list-style `cmd` is joined with ` && `.
    pub cmd: String,
    pub deps: Vec<String>,
    pub params: Vec<ParamRef>,
    pub outs: Vec<String>,
    pub metrics: Vec<String>,
    /// Set when this stage uses a feature we do not expand (foreach, matrix, `${}`).
    pub unsupported: Option<String>,
}

impl Stage {
    /// Every path this stage produces (outs and metrics).
    pub fn produced(&self) -> impl Iterator<Item = &str> {
        self.outs.iter().chain(&self.metrics).map(String::as_str)
    }
}

/// Params a stage tracks from one file. Empty `keys` means "the whole file".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParamRef {
    pub file: String,
    pub keys: Vec<String>,
}

/// `cmd` is either one string or a list of strings, in both `dvc.yaml` and `dvc.lock`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum Cmd {
    One(String),
    Many(Vec<String>),
}

impl Cmd {
    pub fn joined(&self) -> String {
        match self {
            Cmd::One(s) => s.clone(),
            Cmd::Many(list) => list.join(" && "),
        }
    }
}

/// The contents of `dvc.lock`: what each stage looked like the last time it ran.
#[derive(Debug, Default, Deserialize)]
pub struct Lock {
    #[serde(default)]
    pub stages: BTreeMap<String, LockStage>,
}

#[derive(Debug, Deserialize)]
pub struct LockStage {
    pub cmd: Option<Cmd>,
    #[serde(default)]
    pub deps: Vec<LockFile>,
    #[serde(default)]
    pub outs: Vec<LockFile>,
    /// param file -> key -> value. Keys are dotted (`train.lr`) or whole sections (`download`).
    #[serde(default)]
    pub params: BTreeMap<String, BTreeMap<String, Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LockFile {
    pub path: String,
    pub md5: Option<String>,
    pub size: Option<u64>,
    pub nfiles: Option<u64>,
}

// ---------------------------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------------------------

impl Project {
    /// Load `dvc.yaml` (required) and `dvc.lock` (optional) from `root`.
    pub fn load(root: &Path) -> Result<Self> {
        let yaml_path = root.join(DVC_YAML);
        if !yaml_path.is_file() {
            bail!("no {DVC_YAML} found in {}", root.display());
        }
        let text = fs::read_to_string(&yaml_path)
            .with_context(|| format!("cannot read {}", yaml_path.display()))?;
        let stages = parse_dvc_yaml(&text)
            .with_context(|| format!("cannot parse {}", yaml_path.display()))?;
        let lock = Lock::load(&root.join(DVC_LOCK))?;
        Ok(Self {
            root: root.to_path_buf(),
            stages,
            lock,
        })
    }
}

impl Lock {
    /// A missing `dvc.lock` is normal (nothing has run yet) and gives an empty lock.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.is_file() {
            return Ok(Self::default());
        }
        let text =
            fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
        serde_norway::from_str(&text).with_context(|| format!("cannot parse {}", path.display()))
    }
}

/// Load a param file as a generic JSON-like tree. Supports `.json`, `.yaml` and `.yml`.
pub fn load_param_file(root: &Path, file: &str) -> Result<Value> {
    let path = root.join(file);
    let text =
        fs::read_to_string(&path).with_context(|| format!("cannot read {}", path.display()))?;
    let parsed = match path.extension().and_then(|e| e.to_str()) {
        Some("json") => serde_json::from_str(&text).map_err(anyhow::Error::from),
        Some("yaml" | "yml") => serde_norway::from_str(&text).map_err(anyhow::Error::from),
        _ => bail!("unsupported param file type: {}", path.display()),
    };
    parsed.with_context(|| format!("cannot parse {}", path.display()))
}

// ---------------------------------------------------------------------------------------------
// dvc.yaml parsing
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RawDvcYaml {
    // `Mapping` keeps the order of keys in the file; a BTreeMap would sort them.
    stages: Option<serde_norway::Mapping>,
}

#[derive(Debug, Deserialize)]
struct RawStage {
    cmd: Option<Cmd>,
    #[serde(default)]
    deps: Vec<String>,
    #[serde(default)]
    params: Vec<RawParam>,
    #[serde(default)]
    outs: Vec<RawPath>,
    #[serde(default)]
    metrics: Vec<RawPath>,
    foreach: Option<serde_norway::Value>,
    matrix: Option<serde_norway::Value>,
}

/// `- data/x.csv` or `- data/x.csv: {cache: false}`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawPath {
    Plain(String),
    WithOptions(BTreeMap<String, serde_norway::Value>),
}

impl RawPath {
    fn into_path(self) -> Result<String> {
        match self {
            RawPath::Plain(path) => Ok(path),
            RawPath::WithOptions(map) => match map.into_keys().next() {
                Some(path) => Ok(path),
                None => bail!("empty output entry"),
            },
        }
    }
}

/// `- train.lr` (key in params.yaml) or `- params.json: [download]` / `- params.json:` (whole file).
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawParam {
    Key(String),
    File(BTreeMap<String, Option<Vec<String>>>),
}

impl RawParam {
    fn into_refs(self) -> Vec<ParamRef> {
        match self {
            RawParam::Key(key) => vec![ParamRef {
                file: DEFAULT_PARAMS_FILE.to_owned(),
                keys: vec![key],
            }],
            RawParam::File(map) => map
                .into_iter()
                .map(|(file, keys)| ParamRef {
                    file,
                    keys: keys.unwrap_or_default(),
                })
                .collect(),
        }
    }
}

/// Parse the text of a `dvc.yaml` into stages, in file order.
pub fn parse_dvc_yaml(text: &str) -> Result<Vec<Stage>> {
    let raw: RawDvcYaml = serde_norway::from_str(text)?;
    let mut stages = Vec::new();
    for (key, value) in raw.stages.unwrap_or_default() {
        let Some(name) = key.as_str().map(str::to_owned) else {
            bail!("stage names must be strings, found {key:?}");
        };
        let raw_stage: RawStage = serde_norway::from_value(value)
            .with_context(|| format!("invalid definition of stage '{name}'"))?;
        let stage = build_stage(name.clone(), raw_stage)
            .with_context(|| format!("invalid definition of stage '{name}'"))?;
        stages.push(stage);
    }
    Ok(stages)
}

fn build_stage(name: String, raw: RawStage) -> Result<Stage> {
    let unsupported = if raw.foreach.is_some() || raw.matrix.is_some() {
        Some("foreach/matrix stages are not expanded".to_owned())
    } else {
        None
    };
    let outs = raw
        .outs
        .into_iter()
        .map(RawPath::into_path)
        .collect::<Result<Vec<_>>>()?;
    let metrics = raw
        .metrics
        .into_iter()
        .map(RawPath::into_path)
        .collect::<Result<Vec<_>>>()?;
    let cmd = raw.cmd.map(|c| c.joined()).unwrap_or_default();
    let params = raw
        .params
        .into_iter()
        .flat_map(RawParam::into_refs)
        .collect();

    let uses_interpolation = std::iter::once(&cmd)
        .chain(&raw.deps)
        .chain(&outs)
        .chain(&metrics)
        .any(|s| s.contains("${"));
    let unsupported = unsupported.or_else(|| {
        uses_interpolation.then(|| "uses ${} interpolation, which is not expanded".to_owned())
    });

    Ok(Stage {
        name,
        cmd,
        deps: raw.deps,
        params,
        outs,
        metrics,
        unsupported,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    #[test]
    fn loads_demo_fixture_in_file_order() {
        let project = Project::load(&fixture("demo")).unwrap();
        let names: Vec<_> = project.stages.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["prepare", "features", "train"]);

        let train = &project.stages[2];
        assert_eq!(train.cmd, "python3 src/train.py");
        assert_eq!(train.deps, ["src/train.py", "data/features.csv"]);
        assert_eq!(train.outs, ["model.txt"]);
        assert_eq!(train.metrics, ["metrics.json"]);
        assert_eq!(
            train.params,
            [
                ParamRef {
                    file: "params.yaml".into(),
                    keys: vec!["train.lr".into()]
                },
                ParamRef {
                    file: "params.yaml".into(),
                    keys: vec!["train.epochs".into()]
                },
            ]
        );
        assert_eq!(train.unsupported, None);
    }

    #[test]
    fn demo_lock_has_flat_dotted_params_and_file_hashes() {
        let project = Project::load(&fixture("demo")).unwrap();
        let train = &project.lock.stages["train"];
        assert_eq!(train.cmd.as_ref().unwrap().joined(), "python3 src/train.py");
        let params = &train.params["params.yaml"];
        assert_eq!(params["train.epochs"], serde_json::json!(5));
        assert_eq!(params["train.lr"], serde_json::json!(0.01));
        assert!(train.deps.iter().all(|d| d.md5.is_some()));
    }

    #[test]
    fn loads_articleiv_files_with_whole_section_params() {
        let root = fixture("articleiv");
        let project = Project::load(&root).unwrap();
        assert_eq!(project.stages.len(), 1);
        let download = &project.stages[0];
        assert_eq!(download.name, "download");
        assert_eq!(
            download.outs,
            ["data/catalog.json", "errors.jsonl", "pdf_files"]
        );
        assert_eq!(
            download.params,
            [ParamRef {
                file: "params.json".into(),
                keys: vec!["download".into()]
            }]
        );

        // The lock records a whole section; the workspace file differs (`limit`): the stale demo case.
        let locked = &project.lock.stages["download"].params["params.json"]["download"];
        let current = load_param_file(&root, "params.json").unwrap();
        assert_eq!(locked["limit"], serde_json::json!(3));
        assert_eq!(current["download"]["limit"], Value::Null);
        // The directory output carries a file count.
        let pdf = project.lock.stages["download"]
            .outs
            .iter()
            .find(|o| o.path == "pdf_files")
            .unwrap();
        assert_eq!(pdf.nfiles, Some(3));
    }

    #[test]
    fn parses_list_cmd_option_outs_and_param_file_forms() {
        let text = r#"
stages:
  a:
    cmd:
      - echo one
      - echo two
    outs:
      - plain.txt
      - cached.txt:
          cache: false
    params:
      - top.key
      - other.json:
          - sec.a
          - sec.b
      - whole.yaml:
"#;
        let stages = parse_dvc_yaml(text).unwrap();
        let a = &stages[0];
        assert_eq!(a.cmd, "echo one && echo two");
        assert_eq!(a.outs, ["plain.txt", "cached.txt"]);
        assert_eq!(a.params.len(), 3);
        assert_eq!(a.params[1].keys, ["sec.a", "sec.b"]);
        assert!(a.params[2].keys.is_empty());
    }

    #[test]
    fn flags_foreach_and_interpolation_as_unsupported() {
        let text = r#"
stages:
  each:
    foreach: [a, b]
    do:
      cmd: echo ${item}
  vars:
    cmd: echo ${x}
"#;
        let stages = parse_dvc_yaml(text).unwrap();
        assert!(stages[0].unsupported.as_ref().unwrap().contains("foreach"));
        assert!(stages[1].unsupported.as_ref().unwrap().contains("${}"));
    }

    #[test]
    fn missing_dvc_yaml_names_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        let err = Project::load(dir.path()).unwrap_err().to_string();
        assert!(err.contains("no dvc.yaml found"), "{err}");
        assert!(err.contains(&dir.path().display().to_string()), "{err}");
    }

    #[test]
    fn malformed_dvc_yaml_reports_path_and_reason() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(DVC_YAML), "stages: [unclosed").unwrap();
        let err = format!("{:#}", Project::load(dir.path()).unwrap_err());
        assert!(err.contains("cannot parse"), "{err}");
        assert!(err.contains("dvc.yaml"), "{err}");
        assert!(
            err.len() > "cannot parse dvc.yaml".len() + 10,
            "reason missing: {err}"
        );
    }

    #[test]
    fn missing_lock_is_empty_but_malformed_lock_errors() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(DVC_YAML), "stages: {}\n").unwrap();
        let project = Project::load(dir.path()).unwrap();
        assert!(project.lock.stages.is_empty());

        fs::write(dir.path().join(DVC_LOCK), "stages: [oops").unwrap();
        let err = format!("{:#}", Project::load(dir.path()).unwrap_err());
        assert!(err.contains("dvc.lock"), "{err}");
    }

    #[test]
    fn param_files_load_from_json_and_yaml() {
        let demo = load_param_file(&fixture("demo"), "params.yaml").unwrap();
        assert_eq!(demo["train"]["epochs"], serde_json::json!(5));
        let err = load_param_file(&fixture("demo"), "dvc.lock.toml").unwrap_err();
        assert!(format!("{err:#}").contains("cannot read"));
    }
}
