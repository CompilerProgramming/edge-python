use anyhow::{anyhow, bail, Context, Result};
use compiler::modules::{parse_manifest, rules};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

use crate::web::SYSTEM_MODULES;

/* The manifest as `edge add` edits it, the registry fields, `imports`, and every other key kept as written. */
#[derive(Default, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub docs: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edge: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub imports: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extends: Option<String>,
    #[serde(flatten)]
    rest: serde_json::Map<String, serde_json::Value>,
}

impl Manifest {
    /// Load the manifest, or an empty one when the file is absent.
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let manifest: Self = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        // The engine's rules, the ones the registry runs, so a build never packs what it refuses.
        let at = path.display();
        let parsed = parse_manifest(text.as_bytes()).map_err(|e| anyhow!("edge.json at '{at}': {e}"))?;
        rules::check(&parsed, SYSTEM_MODULES).map_err(|e| anyhow!("edge.json at '{at}': {e}"))?;
        Ok(manifest)
    }

    /* Refuses a project written for a newer engine, since a later one keeps running what an earlier one wrote but never the other way. Said here so an old binary names the version it lacks instead of failing on a field it cannot read. */
    pub fn check_engine(path: &Path) -> Result<()> {
        let Some(floor) = Self::load(path)?.edge else { return Ok(()) };
        if rules::newer(&floor, rules::ENGINE) {
            bail!("this project needs edge {floor}, this is {}\nhelp: curl -fsSL https://cdn.edgepython.com/cli/install.sh | sh", rules::ENGINE);
        }
        Ok(())
    }

    /* What the manifest grants each package, its permissions section as written. */
    pub(crate) fn permissions(&self) -> Option<&serde_json::Value> {
        self.rest.get("permissions")
    }

    /// Write the manifest back as pretty JSON with a trailing newline.
    pub(crate) fn save(&self, path: &Path) -> Result<()> {
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(path, format!("{text}\n")).with_context(|| format!("writing {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(body: &str) -> Result<Manifest> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edge.json");
        std::fs::write(&path, body).unwrap();
        Manifest::load(&path)
    }

    #[test]
    fn the_registry_fields_round_trip() {
        let m = load(r#"{ "name": "slugify", "version": "0.1.0", "description": "Turn text into a slug.", "repository": "https://github.com/x/slugify", "docs": "./docs" }"#).unwrap();
        assert_eq!(m.name.as_deref(), Some("slugify"));
        assert_eq!(m.version.as_deref(), Some("0.1.0"));
        assert_eq!(m.repository.as_deref(), Some("https://github.com/x/slugify"));
        assert_eq!(m.docs.as_deref(), Some("./docs"));
    }

    #[test]
    fn a_manifest_without_the_registry_fields_still_loads() {
        let m = load(r#"{ "imports": { "json": "https://x/json.wasm" } }"#).unwrap();
        assert!(m.name.is_none() && m.version.is_none() && m.docs.is_none());
    }

    /* The rules live in the engine, the CLI applies them and names the file it read. */
    #[test]
    fn the_fields_a_registry_would_turn_away_are_refused() {
        for (body, want) in [
            (r#"{ "name": "Slugify" }"#, "must be lowercase"),
            (r#"{ "name": "main" }"#, "is reserved for permissions"),
            (r#"{ "name": "time" }"#, "is reserved for the system module"),
            (r#"{ "version": "01.0.0" }"#, "must be major.minor.patch"),
            (r#"{ "description": "Turn absolutely any text that you have into a tidy url slug fast." }"#, "the cap is 60"),
            (r#"{ "imports": { "net": "./net.py" } }"#, "takes the name of a system module"),
        ] {
            let Err(e) = load(body) else { panic!("{body} should be refused") };
            let err = format!("{e:#}");
            assert!(err.contains("edge.json at '") && err.contains(want), "{body} wanted '{want}', got '{err}'");
        }
    }

    /* A later engine runs what an earlier one wrote, so only a floor above the running version is refused, and a project that names none runs anywhere. */
    #[test]
    fn a_project_runs_on_its_engine_or_a_later_one() {
        let running = rules::parts(rules::ENGINE);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edge.json");
        let at = |major, minor, patch| format!("{{ \"edge\": \"{major}.{minor}.{patch}\" }}");

        for body in ["{}", &at(0, 0, 1), &at(running.0, running.1, running.2)] {
            std::fs::write(&path, body).unwrap();
            assert!(Manifest::check_engine(&path).is_ok(), "{body} should run");
        }

        std::fs::write(&path, at(running.0, running.1, running.2 + 1)).unwrap();
        let err = format!("{:#}", Manifest::check_engine(&path).unwrap_err());
        assert!(err.contains("this project needs edge") && err.contains(rules::ENGINE), "{err}");
    }

    #[test]
    fn unknown_keys_survive_a_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("edge.json");
        std::fs::write(&path, r#"{ "extends": "..", "imports": {}, "future": "kept" }"#).unwrap();
        Manifest::load(&path).unwrap().save(&path).unwrap();
        let back = std::fs::read_to_string(&path).unwrap();
        assert!(back.contains("\"extends\": \"..\""), "{back}");
        assert!(back.contains("\"future\": \"kept\""), "{back}");
    }
}
