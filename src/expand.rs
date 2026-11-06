//! Expanding `foreach`, `matrix`, `vars` and `${...}` references in `dvc.yaml`.
//!
//! Works on the parsed YAML value, before it is turned into typed stages, so everything after
//! it (graph, stale detection, UI) sees ordinary stages with DVC's own names (`name@us`).

use std::{fs, path::Path};

use anyhow::{Result, bail};
use serde_norway::{Mapping, Value};

use crate::project::DEFAULT_PARAMS_FILE;

/// A stage after expansion: its final name and either its definition or why it could not be expanded.
pub type Expanded = (String, Result<Value, String>);

/// Expand every stage of a parsed `dvc.yaml`. Stage order is preserved; an expanded stage's items
/// stay together in the order DVC generates them.
/// `root` is where `vars` files and the default `params.yaml` are looked up (`None`: inline vars only).
pub fn expand(doc: &Value, root: Option<&Path>) -> Result<Vec<Expanded>> {
    let Some(stages) = doc.get("stages") else {
        return Ok(Vec::new());
    };
    let Some(stages) = stages.as_mapping() else {
        if stages.is_null() {
            return Ok(Vec::new());
        }
        bail!("'stages' must be a mapping");
    };

    let mut globals = Mapping::new();
    // DVC makes the default params file available as variables without being asked.
    if let Some(root) = root
        && let Ok(Value::Mapping(params)) = read_mapping(&root.join(DEFAULT_PARAMS_FILE))
    {
        merge(&mut globals, params);
    }
    let vars_error = match doc.get("vars") {
        Some(vars) => add_vars(&mut globals, vars, root).err(),
        None => None,
    };

    let mut out = Vec::new();
    for (key, definition) in stages {
        let Some(name) = key.as_str() else {
            bail!("stage names must be strings, found {key:?}");
        };
        for (stage_name, result) in expand_stage(name, definition, &globals, root) {
            let result = result.map_err(|message| match &vars_error {
                Some(vars) => format!("{message} (while reading vars: {vars})"),
                None => message,
            });
            out.push((stage_name, result));
        }
    }
    Ok(out)
}

fn expand_stage(
    name: &str,
    definition: &Value,
    globals: &Mapping,
    root: Option<&Path>,
) -> Vec<Expanded> {
    let Some(map) = definition.as_mapping() else {
        return vec![(
            name.to_owned(),
            Err("stage definition must be a mapping".to_owned()),
        )];
    };
    let mut local = globals.clone();
    if let Some(vars) = map.get("vars")
        && let Err(error) = add_vars(&mut local, vars, root)
    {
        return vec![(name.to_owned(), Err(format!("stage vars: {error}")))];
    }
    let mut map = map.clone();
    map.remove("vars");

    if map.contains_key("foreach") {
        return expand_foreach(name, &map, &local);
    }
    if map.contains_key("matrix") {
        return expand_matrix(name, &map, &local);
    }
    let result = interpolate(&Value::Mapping(map), &local).map_err(|e| e.to_string());
    vec![(name.to_owned(), result)]
}

fn expand_foreach(name: &str, map: &Mapping, ctx: &Mapping) -> Vec<Expanded> {
    let fail = |message: String| vec![(name.to_owned(), Err(message))];
    let Some(template) = map.get("do") else {
        return fail("foreach stage has no 'do' section".to_owned());
    };
    let foreach = match interpolate(&map["foreach"], ctx) {
        Ok(value) => value,
        Err(error) => return fail(error),
    };

    // (id used in the stage name, `${key}`, `${item}`)
    let items: Vec<(String, Value, Value)> = match &foreach {
        Value::Sequence(list) => list
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let id = match item {
                    Value::Mapping(_) | Value::Sequence(_) => index.to_string(),
                    scalar => scalar_text(scalar).unwrap_or_else(|| index.to_string()),
                };
                (id, Value::from(index as u64), item.clone())
            })
            .collect(),
        Value::Mapping(items) => items
            .iter()
            .map(|(key, item)| {
                let id = scalar_text(key).unwrap_or_default();
                (id, key.clone(), item.clone())
            })
            .collect(),
        _ => return fail("foreach must be a list or a mapping".to_owned()),
    };

    items
        .into_iter()
        .map(|(id, key, item)| {
            let mut scope = ctx.clone();
            scope.insert("item".into(), item);
            scope.insert("key".into(), key);
            let result = interpolate(template, &scope).map_err(|e| e.to_string());
            (format!("{name}@{id}"), result)
        })
        .collect()
}

fn expand_matrix(name: &str, map: &Mapping, ctx: &Mapping) -> Vec<Expanded> {
    let fail = |message: String| vec![(name.to_owned(), Err(message))];
    let matrix = match interpolate(&map["matrix"], ctx) {
        Ok(Value::Mapping(matrix)) => matrix,
        Ok(_) => return fail("matrix must be a mapping of variable to list".to_owned()),
        Err(error) => return fail(error),
    };
    let mut axes: Vec<(Value, Vec<Value>)> = Vec::new();
    for (variable, values) in &matrix {
        let Value::Sequence(values) = values else {
            return fail(format!(
                "matrix variable '{}' must be a list",
                scalar_text(variable).unwrap_or_default()
            ));
        };
        axes.push((variable.clone(), values.clone()));
    }

    let mut template = map.clone();
    template.remove("matrix");
    let template = Value::Mapping(template);

    // Cartesian product; the first variable changes slowest, as in DVC.
    let mut combinations: Vec<Vec<Value>> = vec![Vec::new()];
    for (_, values) in &axes {
        combinations = combinations
            .into_iter()
            .flat_map(|prefix| {
                values.iter().map(move |value| {
                    let mut next = prefix.clone();
                    next.push(value.clone());
                    next
                })
            })
            .collect();
    }

    combinations
        .into_iter()
        .map(|values| {
            let id = values
                .iter()
                .map(|v| scalar_text(v).unwrap_or_default())
                .collect::<Vec<_>>()
                .join("-");
            let item: Mapping = axes
                .iter()
                .map(|(variable, _)| variable.clone())
                .zip(values)
                .collect();
            let mut scope = ctx.clone();
            scope.insert("item".into(), Value::Mapping(item));
            let result = interpolate(&template, &scope).map_err(|e| e.to_string());
            (format!("{name}@{id}"), result)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Variables
// ---------------------------------------------------------------------------------------------

fn add_vars(into: &mut Mapping, vars: &Value, root: Option<&Path>) -> Result<(), String> {
    let Value::Sequence(entries) = vars else {
        return Err("'vars' must be a list".to_owned());
    };
    for entry in entries {
        match entry {
            Value::Mapping(map) => merge(into, map.clone()),
            Value::String(file) => {
                if file.contains(':') {
                    return Err(format!(
                        "'{file}': importing selected keys is not supported"
                    ));
                }
                let Some(root) = root else {
                    return Err(format!("cannot read '{file}' without a project directory"));
                };
                match read_mapping(&root.join(file)) {
                    Ok(Value::Mapping(map)) => merge(into, map),
                    Ok(_) => return Err(format!("'{file}' is not a mapping")),
                    Err(error) => return Err(format!("cannot read '{file}': {error}")),
                }
            }
            other => return Err(format!("unsupported vars entry {other:?}")),
        }
    }
    Ok(())
}

/// YAML and JSON (JSON is YAML) only.
fn read_mapping(path: &Path) -> Result<Value> {
    let text = fs::read_to_string(path)?;
    Ok(serde_norway::from_str(&text)?)
}

fn merge(into: &mut Mapping, from: Mapping) {
    for (key, value) in from {
        into.insert(key, value);
    }
}

// ---------------------------------------------------------------------------------------------
// ${...} substitution
// ---------------------------------------------------------------------------------------------

/// Replace every `${name}` in the strings (and mapping keys) of `value`.
/// A string that is exactly one reference takes the referenced value's type (a list stays a list).
pub fn interpolate(value: &Value, ctx: &Mapping) -> Result<Value, String> {
    match value {
        Value::String(text) => interpolate_string(text, ctx),
        Value::Sequence(items) => items
            .iter()
            .map(|item| interpolate(item, ctx))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Sequence),
        Value::Mapping(map) => {
            let mut out = Mapping::new();
            for (key, item) in map {
                let key = match key {
                    Value::String(text) => match interpolate_string(text, ctx)? {
                        Value::String(text) => Value::String(text),
                        other => scalar_text(&other)
                            .map(Value::String)
                            .ok_or_else(|| format!("a key in '{text}' resolved to a non-scalar"))?,
                    },
                    other => other.clone(),
                };
                out.insert(key, interpolate(item, ctx)?);
            }
            Ok(Value::Mapping(out))
        }
        other => Ok(other.clone()),
    }
}

fn interpolate_string(text: &str, ctx: &Mapping) -> Result<Value, String> {
    if !text.contains("${") {
        return Ok(Value::String(text.to_owned()));
    }
    // Exactly one reference and nothing else: keep the type.
    if let Some(inner) = text
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'))
        && !inner.contains("${")
        && !inner.contains('}')
    {
        return lookup(ctx, inner.trim())
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| Err(unknown_variable(inner.trim())));
    }

    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find("${") {
        // `\${` is an escaped, literal `${`.
        if rest[..start].ends_with('\\') {
            out.push_str(&rest[..start - 1]);
            out.push_str("${");
            rest = &rest[start + 2..];
            continue;
        }
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(end) = after.find('}') else {
            return Err(format!("unterminated '${{' in '{text}'"));
        };
        let expr = after[..end].trim();
        let value = lookup(ctx, expr).ok_or_else(|| unknown_variable(expr))?;
        let piece = scalar_text(value).ok_or_else(|| {
            format!("${{{expr}}} is a list or mapping and cannot be used inside text")
        })?;
        out.push_str(&piece);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(Value::String(out))
}

fn unknown_variable(expr: &str) -> String {
    if expr
        .chars()
        .all(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '[' | ']'))
    {
        format!("unknown variable '${{{expr}}}'")
    } else {
        format!("unsupported expression '${{{expr}}}'")
    }
}

/// `a.b[0].c` -> the value at that path.
fn lookup<'a>(ctx: &'a Mapping, expr: &str) -> Option<&'a Value> {
    let mut parts = expr.split('.');
    let first = parts.next()?;
    let (name, indexes) = split_indexes(first)?;
    let mut current = ctx.get(name)?;
    for index in indexes {
        current = current.get(index)?;
    }
    for part in parts {
        let (name, indexes) = split_indexes(part)?;
        current = current.get(name)?;
        for index in indexes {
            current = current.get(index)?;
        }
    }
    Some(current)
}

/// `name[1][2]` -> (`name`, [1, 2]).
fn split_indexes(part: &str) -> Option<(&str, Vec<usize>)> {
    let Some(open) = part.find('[') else {
        return Some((part, Vec::new()));
    };
    let name = &part[..open];
    let mut indexes = Vec::new();
    for chunk in part[open..].split('[').skip(1) {
        indexes.push(chunk.strip_suffix(']')?.parse().ok()?);
    }
    Some((name, indexes))
}

/// How a scalar reads inside a string; `None` for lists, mappings and null.
fn scalar_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(text: &str) -> Value {
        serde_norway::from_str(text).unwrap()
    }

    fn names(expanded: &[Expanded]) -> Vec<&str> {
        expanded.iter().map(|(name, _)| name.as_str()).collect()
    }

    fn cmd_of(expanded: &Expanded) -> String {
        expanded.1.as_ref().unwrap()["cmd"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    #[test]
    fn plain_stages_pass_through_with_variables_substituted() {
        let doc = doc(
            "vars:\n  - region: eu\nstages:\n  a:\n    cmd: run ${region}\n  b:\n    cmd: plain\n",
        );
        let out = expand(&doc, None).unwrap();
        assert_eq!(names(&out), ["a", "b"]);
        assert_eq!(cmd_of(&out[0]), "run eu");
        assert_eq!(cmd_of(&out[1]), "plain");
    }

    #[test]
    fn foreach_over_scalars_uses_the_value_in_the_name() {
        let doc = doc(
            "stages:\n  each:\n    foreach: [us, eu]\n    do:\n      cmd: run ${item}\n      outs:\n        - out-${item}.txt\n",
        );
        let out = expand(&doc, None).unwrap();
        assert_eq!(names(&out), ["each@us", "each@eu"]);
        assert_eq!(cmd_of(&out[1]), "run eu");
        assert_eq!(
            out[0].1.as_ref().unwrap()["outs"][0].as_str(),
            Some("out-us.txt")
        );
    }

    #[test]
    fn foreach_over_a_mapping_uses_the_key_and_item_fields() {
        let doc = doc(
            "stages:\n  rich:\n    foreach:\n      a: {n: 1}\n      b: {n: 2}\n    do:\n      cmd: run ${key} ${item.n}\n",
        );
        let out = expand(&doc, None).unwrap();
        assert_eq!(names(&out), ["rich@a", "rich@b"]);
        assert_eq!(cmd_of(&out[1]), "run b 2");
    }

    #[test]
    fn foreach_over_a_list_of_mappings_is_numbered() {
        let doc = doc(
            "stages:\n  listed:\n    foreach:\n      - {n: 5}\n      - {n: 6}\n    do:\n      cmd: run ${item.n}\n",
        );
        let out = expand(&doc, None).unwrap();
        assert_eq!(names(&out), ["listed@0", "listed@1"]);
        assert_eq!(cmd_of(&out[1]), "run 6");
    }

    #[test]
    fn matrix_is_a_product_with_the_first_variable_slowest() {
        let doc = doc(
            "stages:\n  grid:\n    matrix:\n      os: [lin, win]\n      ver: [1, 2]\n    cmd: run ${item.os} ${item.ver}\n",
        );
        let out = expand(&doc, None).unwrap();
        assert_eq!(
            names(&out),
            ["grid@lin-1", "grid@lin-2", "grid@win-1", "grid@win-2"]
        );
        assert_eq!(cmd_of(&out[2]), "run win 1");
        assert!(out[0].1.as_ref().unwrap().get("matrix").is_none());
    }

    #[test]
    fn foreach_can_iterate_over_a_variable_and_keeps_its_type() {
        let doc = doc(
            "vars:\n  - regions: [us, eu]\nstages:\n  each:\n    foreach: ${regions}\n    do:\n      cmd: run ${item}\n",
        );
        assert_eq!(names(&expand(&doc, None).unwrap()), ["each@us", "each@eu"]);
    }

    #[test]
    fn a_whole_string_reference_keeps_lists_but_text_cannot_embed_them() {
        let doc = doc(
            "vars:\n  - files: [a.csv, b.csv]\nstages:\n  ok:\n    cmd: x\n    deps: ${files}\n  bad:\n    cmd: run ${files}\n",
        );
        let out = expand(&doc, None).unwrap();
        assert_eq!(
            out[0].1.as_ref().unwrap()["deps"][1].as_str(),
            Some("b.csv")
        );
        assert!(out[1].1.as_ref().unwrap_err().contains("list or mapping"));
    }

    #[test]
    fn unknown_and_unsupported_references_fail_only_their_stage() {
        let doc = doc(
            "stages:\n  good:\n    cmd: fine\n  unknown:\n    cmd: run ${nope}\n  fancy:\n    cmd: run ${a:b}\n",
        );
        let out = expand(&doc, None).unwrap();
        assert!(out[0].1.is_ok());
        assert!(
            out[1]
                .1
                .as_ref()
                .unwrap_err()
                .contains("unknown variable '${nope}'")
        );
        assert!(
            out[2]
                .1
                .as_ref()
                .unwrap_err()
                .contains("unsupported expression")
        );
    }

    #[test]
    fn escaped_references_stay_literal() {
        let doc = doc("stages:\n  a:\n    cmd: echo \\${HOME}\n");
        assert_eq!(cmd_of(&expand(&doc, None).unwrap()[0]), "echo ${HOME}");
    }

    #[test]
    fn indexes_and_nested_paths_resolve() {
        let doc = doc(
            "vars:\n  - cfg: {sizes: [10, 20], name: x}\nstages:\n  a:\n    cmd: run ${cfg.name} ${cfg.sizes[1]}\n",
        );
        assert_eq!(cmd_of(&expand(&doc, None).unwrap()[0]), "run x 20");
    }

    #[test]
    fn stage_level_vars_apply_to_that_stage_only() {
        let doc = doc(
            "stages:\n  a:\n    vars:\n      - who: me\n    cmd: hi ${who}\n  b:\n    cmd: hi ${who}\n",
        );
        let out = expand(&doc, None).unwrap();
        assert_eq!(cmd_of(&out[0]), "hi me");
        assert!(out[1].1.is_err());
    }

    #[test]
    fn vars_files_and_the_default_params_file_are_read_from_the_project() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("params.yaml"), "lr: 0.5\n").unwrap();
        fs::write(dir.path().join("extra.json"), "{\"mode\": \"fast\"}").unwrap();
        let doc = doc("vars:\n  - extra.json\nstages:\n  a:\n    cmd: run ${mode} ${lr}\n");
        let out = expand(&doc, Some(dir.path())).unwrap();
        assert_eq!(cmd_of(&out[0]), "run fast 0.5");
    }

    #[test]
    fn a_missing_vars_file_is_mentioned_when_a_reference_then_fails() {
        let dir = tempfile::tempdir().unwrap();
        let doc = doc("vars:\n  - gone.yaml\nstages:\n  a:\n    cmd: run ${mode}\n");
        let error = expand(&doc, Some(dir.path())).unwrap()[0]
            .1
            .clone()
            .unwrap_err();
        assert!(
            error.contains("unknown variable") && error.contains("gone.yaml"),
            "{error}"
        );
    }

    #[test]
    fn keys_are_substituted_too() {
        let doc = doc(
            "stages:\n  each:\n    foreach: [x]\n    do:\n      cmd: c\n      outs:\n        - ${item}.csv:\n            cache: false\n",
        );
        let out = expand(&doc, None).unwrap();
        let outs = &out[0].1.as_ref().unwrap()["outs"][0];
        assert!(outs.get("x.csv").is_some(), "{outs:?}");
    }

    #[test]
    fn foreach_without_do_is_reported() {
        let doc = doc("stages:\n  each:\n    foreach: [a]\n");
        assert!(
            expand(&doc, None).unwrap()[0]
                .1
                .as_ref()
                .unwrap_err()
                .contains("'do'")
        );
    }
}
