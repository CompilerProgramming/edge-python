use anyhow::{anyhow, bail, Result};
use compiler::modules::lock::{locked_spec, verify_pin};
use compiler::modules::rules::HOLDERS;
use compiler::modules::{dir_of, join_relative, parse_integrity, walk_up_dirs};
use compiler::util::sha256::{hex_encode, sha256};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
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

/* A package in the tree, the name the root grants it by and what it asks. */
pub struct Package {
    pub name: String,
    label: String,
    via: Vec<String>,
    pub section: Value,
}

impl Package {
    /// The package and the packages it came through, as a report names it.
    pub fn who(&self) -> String {
        match self.via.is_empty() {
            true => self.label.clone(),
            false => format!("{}, via {}", self.label, self.via.join(" > ")),
        }
    }
}

/* Stops when the root edge.json misses what a package in its tree asks for. */
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
        let missing = system::unmet(&grants, &package.name, &package.section);
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

/* Every package the imports reach, each through its own edge.json and edge.lock. */
pub fn packages(path: &Path, imports: &Map<String, Value>, lock: &Lock) -> Result<Vec<Package>> {
    let project = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    // The root grants, so it is never one of the packages it checks.
    let mut walk = Walk { seen: HashSet::from([key("", "")]), found: Vec::new() };
    walk.imports(&Rc::new(Home::Disk(project.to_path_buf())), "", "", &[], imports, lock)?;
    Ok(walk.found)
}

struct Walk {
    seen: HashSet<String>,
    found: Vec<Package>,
}

impl Walk {
    /* Follows each import of the package at `dir`, as the lock beside its manifest resolved it. */
    fn imports(&mut self, home: &Rc<Home>, id: &str, dir: &str, via: &[String], imports: &Map<String, Value>, lock: &Lock) -> Result<()> {
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
                    self.bundle(target, &spec, &bytes, name, via)?;
                }
                continue;
            }
            let rel = join_relative(dir, target).trim_start_matches("./").to_string();
            if packed(&rel) {
                let bytes = home.read(&rel).ok_or_else(|| anyhow!("reading {rel}: not found"))?;
                self.bundle(&rel, &spec, &bytes, name, via)?;
                continue;
            }
            // A module belongs to the nearest manifest above it, another package unless that is this one.
            let nearest = walk_up_dirs(&dir_of(&rel)).find(|d| d == dir || home.read(&format!("{d}edge.json")).is_some());
            if let Some(found) = nearest.filter(|d| d != dir) {
                let place = format!("{found}edge.json");
                self.visit(home, id, &found, &place, name, via)?;
            }
        }
        Ok(())
    }

    /* A packed package, held to its pin, then read from inside itself like any other. */
    fn bundle(&mut self, address: &str, spec: &str, bytes: &[u8], name: &str, via: &[String]) -> Result<()> {
        verify_pin(spec, bytes).map_err(|e| anyhow!(e))?;
        let files = crate::pack::into_files(Bundle::decode(bytes).map_err(|e| anyhow!("package '{address}' is not a packed .edge, {e}"))?);
        self.visit(&Rc::new(Home::Packed(files)), &hex_encode(&sha256(bytes)), "", address, name, via)
    }

    /* Records the package whose manifest sits at `dir`, then follows what it imports in turn. */
    fn visit(&mut self, home: &Rc<Home>, id: &str, dir: &str, place: &str, hint: &str, via: &[String]) -> Result<()> {
        if !self.seen.insert(key(id, dir)) {
            return Ok(());
        }
        let Some(bytes) = home.read(&format!("{dir}edge.json")) else { return Ok(()) };
        let manifest: Value = serde_json::from_slice(&bytes).map_err(|e| anyhow!("edge.json at '{place}': {e}"))?;
        let named = manifest.get("name").and_then(Value::as_str);
        if let Some(name) = named.filter(|n| HOLDERS.contains(n)) {
            bail!("edge.json at '{place}': name '{name}' is reserved for permissions");
        }
        let name = named.unwrap_or(hint).to_string();
        let section = manifest.get("permissions").cloned().unwrap_or(Value::Null);
        if !section.is_null() {
            if let Some(problem) = system::check(&section) {
                bail!("edge.json at '{place}': {problem}");
            }
            // The root grants by name, and against an empty grant every ask comes back unmet.
            if named.is_none() && !system::unmet(&json!({}), &name, &section).is_empty() {
                bail!("edge.json at '{place}': a package that asks for permissions needs a name");
            }
        }
        let label = match manifest.get("version").and_then(Value::as_str) {
            Some(version) => format!("{name} {version}"),
            None => name.clone(),
        };
        let lock = match home.read(&format!("{dir}{}", lock::FILE)) {
            Some(bytes) => Lock::parse(&bytes).map_err(|e| anyhow!("edge.json at '{place}': {e}"))?,
            None => Lock::default(),
        };
        let imports = manifest.get("imports").and_then(Value::as_object).cloned().unwrap_or_default();
        let chain: Vec<String> = via.iter().cloned().chain([label.clone()]).collect();
        self.found.push(Package { name, label, via: via.to_vec(), section });
        self.imports(home, id, dir, &chain, &imports, &lock)
    }
}

fn key(id: &str, dir: &str) -> String {
    format!("{id}:{dir}")
}

/* Whether a target is a packed .edge, the one artifact carrying its own manifest. */
fn packed(target: &str) -> bool {
    target.split(['?', '#']).next().is_some_and(|path| path.ends_with(".edge"))
}
