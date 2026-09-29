use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::s;
use super::json::{self, Value};

/* Parsed `edge.json`, `extends` inherits another manifest's imports when a name is not local. */
#[derive(Clone, Default, Debug)]
pub struct Manifest {
    // Bare name to spec pairs, parsed once and scanned linearly, a Vec avoids a hashbrown monomorphization.
    pub imports: Vec<(String, String)>,
    pub extends: Option<String>,
    // What a package carries into a registry, which the compiler never reads.
    pub name: Option<String>,
    pub version: Option<String>,
    pub description: Option<String>,
    pub repository: Option<String>,
    // The lowest engine the manifest runs on.
    pub edge: Option<String>,
    // Each holder with the entries it is granted, as written.
    pub permissions: Option<Vec<(String, Vec<String>)>>,
}

/* Parse an edge.json, unknown keys skipped, numbers and booleans refused. */
pub fn parse_manifest(bytes: &[u8]) -> Result<Manifest, String> {
    let Value::Obj(fields) = json::parse(bytes, "edge.json")? else {
        return Err(s!("edge.json must be a JSON object"));
    };
    let mut m = Manifest::default();
    for (key, value) in fields {
        match key.as_str() {
            "imports" => m.imports = imports_of(value)?,
            "permissions" => m.permissions = Some(holders_of(value)?),
            "extends" => m.extends = Some(string_of(&key, value)?),
            "name" => m.name = Some(string_of(&key, value)?),
            "version" => m.version = Some(string_of(&key, value)?),
            "description" => m.description = Some(string_of(&key, value)?),
            "repository" => m.repository = Some(string_of(&key, value)?),
            "edge" => m.edge = Some(string_of(&key, value)?),
            _ => {}
        }
    }
    Ok(m)
}

fn string_of(key: &str, value: Value) -> Result<String, String> {
    match value {
        Value::Str(s) => Ok(s),
        _ => Err(s!("'", str key, "' must be a string")),
    }
}

fn imports_of(value: Value) -> Result<Vec<(String, String)>, String> {
    let Value::Obj(fields) = value else { return Err(s!("'imports' must be an object")) };
    fields.into_iter().map(|(name, target)| match target {
        Value::Str(target) => Ok((name, target)),
        _ => Err(s!("'imports' maps '", str &name, "' to something that is not a string")),
    }).collect()
}

// The shape the grants check of the system modules reads, each holder to its list of entries.
fn holders_of(value: Value) -> Result<Vec<(String, Vec<String>)>, String> {
    let Value::Obj(fields) = value else { return Err(s!("permissions must map each package to a list of entries")) };
    fields.into_iter().map(|(holder, entries)| {
        let listed = match entries {
            Value::List(items) => items.into_iter().map(|e| match e { Value::Str(e) => Some(e), _ => None }).collect::<Option<Vec<_>>>(),
            _ => None,
        };
        listed.map(|entries| (holder.clone(), entries)).ok_or_else(|| s!("permissions for '", str &holder, "' must be a list of entries such as \"net:api.example.com\""))
    }).collect()
}

/* Yield the directory of `start` and every parent, in order. Each ends in '/' or is "" (topmost). */
pub fn walk_up_dirs(start: &str) -> impl Iterator<Item = String> + '_ {
    let mut current = Some(start.to_string());
    core::iter::from_fn(move || {
        let dir = current.take()?;
        current = parent_dir(&dir);
        Some(dir)
    })
}

/* Directory of `spec`, up to and including the last '/'. A packed package is the directory of the files it carries, "pkg/app.edge" -> "pkg/app.edge/". */
pub fn dir_of(spec: &str) -> String {
    let path = spec.split(['#', '?']).next().unwrap_or(spec);
    if path.ends_with(".edge") {
        return s!(str path, "/");
    }
    match spec.rfind('/') {
        Some(i) => spec[..=i].to_string(),
        None => String::new(),
    }
}

/* Where a run resolves from, `entry` a script path, a directory ending in '/', or empty. */
pub fn entry_dir(entry: &str) -> String {
    // A leading ./ would fork the spec-space with phantom dirs.
    dir_of(entry.trim_start_matches("./"))
}

/* The spec a host registers a system module under for the package whose manifest sits in `dir`. */
pub fn system_spec(name: &str, dir: &str) -> String {
    s!("system:", str name, "@", str dir)
}

/* Resolve `target` against `dir`. Absolute forms pass through, `../` pops parents, `./` strips only when base is non-empty. */
pub fn join_relative(dir: &str, target: &str) -> String {
    if target.contains("://") || target.starts_with('/') {
        return target.to_string();
    }
    let mut base = dir.to_string();
    let mut t = target;
    while let Some(rest) = t.strip_prefix("../") {
        base = parent_dir(&base).unwrap_or_default();
        t = rest;
    }
    if t == ".." { return parent_dir(&base).unwrap_or_default(); }
    if t == "." || t.is_empty() { return base; }
    if !base.is_empty() {
        while let Some(rest) = t.strip_prefix("./") { t = rest; }
        if !base.ends_with('/') { base.push('/'); }
    }
    base.push_str(t);
    base
}

pub fn parent_dir(dir: &str) -> Option<String> {
    if dir.is_empty() { return None; }
    let trimmed = dir.trim_end_matches('/');
    // URL guard, never strip the host. After "scheme://" there must still be a '/' to walk into.
    if let Some(scheme_end) = trimmed.find("://") {
        let after = &trimmed[scheme_end + 3..];
        if !after.contains('/') { return None; }
    }
    match trimmed.rsplit_once('/') {
        Some(("", _)) => Some(String::new()),
        Some((head, _)) => Some(s!(str head, "/")),
        None => Some(String::new()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_relative_routes() {
        assert_eq!(join_relative("lib/test/", "./helper.py"), "lib/test/helper.py");
        assert_eq!(join_relative("lib/test/", "../parse.py"), "lib/parse.py");
        assert_eq!(join_relative("lib/test/", "../../main.py"), "main.py");
        assert_eq!(join_relative("lib/", "../../escape.py"), "escape.py"); // clamped at root
        assert_eq!(join_relative("lib/test/", "/std/x.py"), "/std/x.py");
        assert_eq!(join_relative("", "https://x/y.py"), "https://x/y.py");
    }

    #[test]
    fn permissions_are_read_as_written() {
        let m = parse_manifest(br#"{ "imports": { "a": "./a.py" }, "permissions": { "main": ["net:api.example.com", "time"], "all": [] } }"#).unwrap();
        assert_eq!(m.imports, alloc::vec![(String::from("a"), String::from("./a.py"))]);
        let holders = m.permissions.unwrap();
        assert_eq!(holders[0], (String::from("main"), alloc::vec![String::from("net:api.example.com"), String::from("time")]));
        assert!(parse_manifest(br#"{ "permissions": { "main": [1] } }"#).is_err());
        assert!(parse_manifest(br#"{ "permissions": { "main": ["net" "time"] } }"#).is_err());
        assert!(parse_manifest(br#"{ "permissions": ["net"] }"#).is_err());
    }

    #[test]
    fn the_registry_fields_are_read_and_must_be_strings() {
        let m = parse_manifest(r#"{ "name": "café", "version": "0.1.0", "edge": "0.7.5", "future": { "kept": ["x"] } }"#.as_bytes()).unwrap();
        assert_eq!((m.name.as_deref(), m.version.as_deref(), m.edge.as_deref()), (Some("café"), Some("0.1.0"), Some("0.7.5")));
        assert!(parse_manifest(br#"{ "name": ["x"] }"#).unwrap_err().contains("'name' must be a string"));
    }

    #[test]
    fn a_system_module_is_spelled_per_package() {
        assert_eq!(system_spec("net", ""), "system:net@");
        assert_eq!(system_spec("time", "https://cdn/pkg/clock/0.1.0/app.edge/"), "system:time@https://cdn/pkg/clock/0.1.0/app.edge/");
    }

    #[test]
    fn dir_walk_routes() {
        assert_eq!(dir_of("lib/test/a.py"), "lib/test/");
        assert_eq!(dir_of("a.py"), "");
        assert_eq!(dir_of("https://cdn/pkg/test/0.1.0/app.edge#sha256-00"), "https://cdn/pkg/test/0.1.0/app.edge/");
        assert_eq!(parent_dir("lib/test/"), Some("lib/".into()));
        assert_eq!(parent_dir("lib/"), Some("".into()));
        assert_eq!(parent_dir(""), None);
    }

    #[test]
    fn a_run_resolves_from_its_entry_directory() {
        assert_eq!(entry_dir("sub/main.py"), "sub/");
        assert_eq!(entry_dir("././sub/a_test.py"), "sub/");
        assert_eq!(entry_dir("actors/"), "actors/");
        assert_eq!(entry_dir("main.py"), "");
        assert_eq!(entry_dir(""), "");
    }
}
