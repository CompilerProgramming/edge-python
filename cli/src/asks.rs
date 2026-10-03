use anyhow::{anyhow, bail, Result};
use compiler::modules::lock::{locked_spec, verify_pin};
use compiler::modules::{dir_of, join_relative, parse_integrity, walk_up_dirs};
use compiler::util::sha256::{hex_encode, sha256};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::host::{fetch_cached, system};
use crate::lock::{self, Lock};
use crate::pack::Bundle;

/* Where a package's files are, the project on disk or a packed bundle held in memory. */
enum Home {
    Disk(PathBuf),
    Packed(HashMap<String, Vec<u8>>),
}

impl Home {
    fn read(&self, rel: &str) -> Option<Vec<u8>> {
        match self {
            Home::Disk(root) => std::fs::read(root.join(rel)).ok(),
            Home::Packed(files) => files.get(rel).cloned(),
        }
    }
}

/* A package the root imports, the key it is granted by and what its section lists. */
pub struct Package {
    pub name: String,
    label: String,
    pub section: Value,
}

impl Package {
    /// The package as a report names it, its version beside the key it is granted by.
    pub fn who(&self) -> String {
        self.label.clone()
    }
}

/* Stops when the root edge.json misses what a package it imports lists. */
pub fn check(path: &Path, lock: &Lock) -> Result<()> {
    let Ok(bytes) = std::fs::read(path) else { return Ok(()) };
    let manifest: Value = serde_json::from_slice(&bytes).map_err(|e| anyhow!("parsing {}: {e}", path.display()))?;
    let grants = manifest.get("permissions").cloned().unwrap_or_else(|| json!({}));
    if let Some(problem) = manifest.get("permissions").and_then(system::check) {
        bail!("edge.json at '{}': {problem}", path.display());
    }
    let imports = manifest.get("imports").and_then(Value::as_object).cloned().unwrap_or_default();
    let mut unmet = Vec::new();
    for package in packages(path, &imports, lock)?.iter().filter(|p| !p.section.is_null()) {
        let missing = system::unmet(&system::held(&json!([[grants, package.name]])), &package.section);
        if !missing.is_empty() {
            unmet.push((package.who(), missing.join(", ")));
        }
    }
    if unmet.is_empty() {
        return Ok(());
    }
    let width = unmet.iter().map(|(who, _)| who.len()).max().unwrap_or(0);
    let lines: Vec<String> = unmet.iter().map(|(who, asks)| format!("  {who:<width$}   {asks}")).collect();
    bail!("edge.json does not grant what these packages ask for\n{}\nhelp: grant them under \"permissions\" in edge.json", lines.join("\n"))
}

/* Every package the root imports, each tree below it held to what its importers pass on. */
pub fn packages(path: &Path, imports: &Map<String, Value>, lock: &Lock) -> Result<Vec<Package>> {
    let project = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    // The root grants, so it is never one of the packages it checks.
    let mut walk = Walk { found: Vec::new(), path: vec![key("", "")] };
    walk.imports(&Rc::new(Home::Disk(project.to_path_buf())), "", "", None, imports, lock)?;
    Ok(walk.found)
}

/* A non-root importer, as a report names it, and the section it passes on from. */
struct Importer<'a> {
    label: &'a str,
    section: &'a Value,
}

struct Walk {
    found: Vec<Package>,
    // The packages from the root down to the one being read.
    path: Vec<String>,
}

impl Walk {
    /* Follows each import of the package at `dir`, as the lock beside its manifest resolved it. */
    fn imports(&mut self, home: &Rc<Home>, id: &str, dir: &str, importer: Option<&Importer>, imports: &Map<String, Value>, lock: &Lock) -> Result<()> {
        for (name, target) in imports {
            // A target the lock cannot answer yet is the command's own error to report.
            let Some(spec) = target.as_str().and_then(|t| locked_spec(name, t, Some(lock)).ok()) else { continue };
            let (target, pin) = parse_integrity(&spec).map_err(|e| anyhow!(e))?;
            if target.contains("://") {
                if packed(target) {
                    // A bundle carries what it vendored under the url it answers at.
                    let vendored = match home.as_ref() {
                        Home::Packed(files) => files.get(target).cloned(),
                        Home::Disk(_) => None,
                    };
                    let bytes = match vendored {
                        Some(bytes) => bytes,
                        None => fetch_cached(target, pin).map_err(|e| anyhow!(e))?,
                    };
                    self.bundle(target, &spec, &bytes, name, importer)?;
                }
                continue;
            }
            let rel = join_relative(dir, target).trim_start_matches("./").to_string();
            if packed(&rel) {
                let bytes = home.read(&rel).ok_or_else(|| anyhow!("reading {rel}: not found"))?;
                self.bundle(&rel, &spec, &bytes, name, importer)?;
                continue;
            }
            // A module belongs to the nearest manifest above it, a new package unless it holds the importer.
            let nearest = walk_up_dirs(&dir_of(&rel)).find(|d| d == dir || home.read(&format!("{d}edge.json")).is_some());
            if let Some(found) = nearest.filter(|d| !walk_up_dirs(dir).any(|around| around == *d)) {
                let place = format!("{found}edge.json");
                self.visit(home, id, &found, &place, name, importer)?;
            }
        }
        Ok(())
    }

    /* A packed package, held to its pin, then read from inside itself like any other. */
    fn bundle(&mut self, address: &str, spec: &str, bytes: &[u8], name: &str, importer: Option<&Importer>) -> Result<()> {
        verify_pin(spec, bytes).map_err(|e| anyhow!(e))?;
        let files = crate::pack::into_files(Bundle::decode(bytes).map_err(|e| anyhow!("package '{address}' is not a packed .edge, {e}"))?);
        self.visit(&Rc::new(Home::Packed(files)), &hex_encode(&sha256(bytes)), "", address, name, importer)
    }

    /* Holds the package at `dir` to what its importer passes, then follows its own imports. */
    fn visit(&mut self, home: &Rc<Home>, id: &str, dir: &str, place: &str, name: &str, importer: Option<&Importer>) -> Result<()> {
        let at = key(id, dir);
        // A package importing one that imports it reaches that same package, as the engine reads it.
        if self.path.contains(&at) {
            return Ok(());
        }
        let Some(bytes) = home.read(&format!("{dir}edge.json")) else { return Ok(()) };
        let manifest: Value = serde_json::from_slice(&bytes).map_err(|e| anyhow!("edge.json at '{place}': {e}"))?;
        let section = manifest.get("permissions").cloned().unwrap_or(Value::Null);
        if !section.is_null()
            && let Some(problem) = system::check(&section)
        {
            bail!("edge.json at '{place}': {problem}");
        }
        let label = match manifest.get("version").and_then(Value::as_str) {
            Some(version) => format!("{name} {version}"),
            None => name.to_string(),
        };
        match importer {
            None => self.found.push(Package { name: name.to_string(), label: label.clone(), section: section.clone() }),
            Some(importer) if !section.is_null() => {
                // What the importer writes for it, since the root check covers what the importer holds.
                let missing = system::unmet(&system::held(&json!([[importer.section, name]])), &section);
                if !missing.is_empty() {
                    bail!("edge.json at '{place}' asks for {}, which {} does not pass it", missing.join(", "), importer.label);
                }
            }
            Some(_) => {}
        }
        let lock = match home.read(&format!("{dir}{}", lock::FILE)) {
            Some(bytes) => Lock::parse(&bytes).map_err(|e| anyhow!("edge.json at '{place}': {e}"))?,
            None => Lock::default(),
        };
        let imports = manifest.get("imports").and_then(Value::as_object).cloned().unwrap_or_default();
        let given = if section.is_null() { json!({}) } else { section };
        self.path.push(at);
        let followed = self.imports(home, id, dir, Some(&Importer { label: &label, section: &given }), &imports, &lock);
        self.path.pop();
        followed
    }
}

fn key(id: &str, dir: &str) -> String {
    format!("{id}:{dir}")
}

/* Whether a target is a packed .edge, the one artifact carrying its own manifest. */
fn packed(target: &str) -> bool {
    target.split(['?', '#']).next().is_some_and(|path| path.ends_with(".edge"))
}
