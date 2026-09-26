use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The generated file beside an edge.json, holding what each name it declares resolves to.
pub const FILE: &str = "edge.lock";

/* One resolved name, the address to fetch and the digest those bytes must hash to. A url names itself, so it locks no version. */
#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct Entry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub url: String,
    pub digest: String,
}

/* What a manifest's versions and urls resolved to, written by `edge lock` and read by everything else, so a run never asks the registry where a name points. */
#[derive(Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Lock {
    entries: BTreeMap<String, Entry>,
}

impl Lock {
    /// The lock beside `manifest`, empty when the file is absent.
    pub fn beside(manifest: &Path) -> Result<Self> {
        let path = path_beside(manifest);
        if !path.exists() {
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))
    }

    /// The same lock read out of a packed bundle, whose files are already in memory.
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        serde_json::from_slice(bytes).context("parsing edge.lock")
    }

    pub fn insert(&mut self, name: &str, entry: Entry) {
        self.entries.insert(name.to_string(), entry);
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.get(name)
    }

    /// Write the lock as pretty JSON with a trailing newline, beside `manifest`.
    pub fn save(&self, manifest: &Path) -> Result<PathBuf> {
        let path = path_beside(manifest);
        let text = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, format!("{text}\n")).with_context(|| format!("writing {}", path.display()))?;
        Ok(path)
    }

    /* What a declared target becomes once the lock has spoken, the url and digest spelled as the pinned spec every host already reads. A path stays itself and an already pinned url passes through, so only a version has to be locked. */
    pub fn spec(&self, name: &str, target: &str) -> Result<String> {
        if let Some(version) = version_of(target) {
            let Some(entry) = self.get(name) else {
                bail!("'{name}' is not locked, run edge lock");
            };
            if entry.version.as_deref() != Some(version) {
                let held = entry.version.as_deref().unwrap_or("a url");
                bail!("'{name}' is declared {version} and locked {held}, run edge lock");
            }
            return pinned(name, &entry.url, &entry.digest);
        }
        if target.contains("://")
            && !target.contains("#sha256-")
            && let Some(entry) = self.get(name)
        {
            return pinned(name, &entry.url, &entry.digest);
        }
        Ok(target.to_string())
    }
}

/* Every target a manifest declares as the lock beside it resolved them, so a caller sees addresses and never a version. */
pub fn resolved(manifest: &Path, imports: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>> {
    let lock = Lock::beside(manifest)?;
    imports.iter().map(|(name, target)| Ok((name.clone(), lock.spec(name, target)?))).collect()
}

/// The version a target names, None when it is a path or a url.
pub fn version_of(target: &str) -> Option<&str> {
    let parts: Vec<&str> = target.split('.').collect();
    let plain = parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()));
    plain.then_some(target)
}

/// The digest of `bytes` as a lock records it.
pub fn digest_of(bytes: &[u8]) -> String {
    format!("sha256-{}", compiler::util::sha256::hex_encode(&compiler::util::sha256::sha256(bytes)))
}

/* The url and digest spelled as a pinned spec, checked here so a hand-edited lock names itself rather than the address it holds. */
fn pinned(name: &str, url: &str, digest: &str) -> Result<String> {
    let hex = digest.strip_prefix("sha256-").filter(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
    if hex.is_none() {
        bail!("'{name}' holds '{digest}' in {FILE}, which is not sha256- and 64 hex characters");
    }
    Ok(format!("{url}#{digest}"))
}

fn path_beside(manifest: &Path) -> PathBuf {
    manifest.with_file_name(FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A digest the pin parser accepts, so a fixture never encodes a shape that would fail on a real fetch.
    const DIGEST: &str = "sha256-a4bf0317b809477e6e475cd791573953d3ae05c4e2b03bd5f35199609557dd90";

    fn locked(name: &str, version: Option<&str>) -> Lock {
        let mut lock = Lock::default();
        lock.insert(
            name,
            Entry {
                version: version.map(String::from),
                url: format!("https://cdn.test/pkg/{name}/0.1.0/app.edge"),
                digest: DIGEST.to_string(),
            },
        );
        lock
    }

    #[test]
    fn only_three_numeric_parts_read_as_a_version() {
        for target in ["0.1.0", "10.20.30"] {
            assert_eq!(version_of(target), Some(target));
        }
        for target in ["./lib/util.py", "https://x/y.wasm", "0.1", "0.1.0-rc1", "1.0.0.0", "a.b.c"] {
            assert_eq!(version_of(target), None, "{target}");
        }
    }

    #[test]
    fn a_locked_version_becomes_a_pinned_url() {
        let spec = locked("json", Some("0.1.0")).spec("json", "0.1.0").unwrap();
        assert_eq!(spec, format!("https://cdn.test/pkg/json/0.1.0/app.edge#{DIGEST}"));
    }

    #[test]
    fn a_version_the_lock_does_not_hold_names_the_command_that_writes_it() {
        let err = Lock::default().spec("json", "0.1.0").unwrap_err().to_string();
        assert!(err.contains("'json' is not locked, run edge lock"), "{err}");
    }

    #[test]
    fn a_version_the_lock_holds_at_another_release_is_refused() {
        let err = locked("json", Some("0.1.0")).spec("json", "0.2.0").unwrap_err().to_string();
        assert!(err.contains("declared 0.2.0 and locked 0.1.0"), "{err}");
    }

    // A path carries its own bytes and a pinned url carries its own digest, so neither needs the lock.
    #[test]
    fn a_path_and_a_pinned_url_pass_through_unlocked() {
        let lock = Lock::default();
        for target in ["./lib/util.py", "https://x/y.wasm#sha256-ab"] {
            assert_eq!(lock.spec("util", target).unwrap(), target);
        }
    }

    /* A bare url runs with or without the lock, since it already says where it points, and locking it only adds the digest. */
    #[test]
    fn a_bare_url_gains_its_digest_from_the_lock_and_runs_without_one() {
        assert_eq!(Lock::default().spec("foo", "https://x/y.wasm").unwrap(), "https://x/y.wasm");
        let mut lock = Lock::default();
        lock.insert("foo", Entry { version: None, url: "https://x/y.wasm".to_string(), digest: DIGEST.to_string() });
        assert_eq!(lock.spec("foo", "https://x/y.wasm").unwrap(), format!("https://x/y.wasm#{DIGEST}"));
    }

    // A digest nothing could ever match is refused here, where the lock can be named, rather than at the fetch.
    #[test]
    fn a_digest_that_is_not_a_sha256_names_the_lock() {
        let mut lock = Lock::default();
        lock.insert("json", Entry { version: Some("0.1.0".to_string()), url: "https://x/y.edge".to_string(), digest: "sha256-00".to_string() });
        let err = lock.spec("json", "0.1.0").unwrap_err().to_string();
        assert!(err.contains("holds 'sha256-00' in edge.lock"), "{err}");
    }

    #[test]
    fn a_lock_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("edge.json");
        locked("json", Some("0.1.0")).save(&manifest).unwrap();
        let back = Lock::beside(&manifest).unwrap();
        assert_eq!(back.get("json").unwrap().version.as_deref(), Some("0.1.0"));
        let text = std::fs::read_to_string(dir.path().join(FILE)).unwrap();
        assert!(text.ends_with("}\n"), "{text}");
        assert!(!text.contains("entries"), "the map is the whole file: {text}");
    }

    #[test]
    fn a_missing_lock_reads_as_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Lock::beside(&dir.path().join("edge.json")).unwrap().get("json").is_none());
    }
}
