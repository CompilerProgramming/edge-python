use anyhow::{anyhow, bail, Context, Result};
use std::fs;
use std::path::Path;

use crate::host::site;

/* Sends a packed `.edge` to the registry. The artifact is the whole request, so the registry reads the name, the license and what the bundle carries out of the bytes that will run, and nothing here declares it a second time. */
pub fn run(artifact: &Path) -> Result<()> {
    let token = std::env::var("EDGE_TOKEN").map_err(|_| anyhow!("set EDGE_TOKEN to a token from edgepython.com/settings#tokens"))?;

    let bytes = fs::read(artifact).with_context(|| format!("reading {}", artifact.display()))?;
    let name = artifact.file_name().unwrap_or(artifact.as_os_str()).to_string_lossy().into_owned();
    if let Some(path) = javascript_in(&bytes) {
        bail!("{name} carries '{path}', which is JavaScript, ship a .py or a .wasm");
    }

    let sp = crate::ui::spinner(&format!("publishing {name}"));

    match send(&token, &bytes) {
        Ok(published) => {
            sp.done(&format!("published {} {}", published.name, published.version));
            crate::ui::note(&format!("add it with  edge add {}", published.name));
            crate::ui::note(&published.url);
            Ok(())
        }
        // A version is never overwritten, and its own exit code lets a script tell that from a failure.
        Err(e) if already_published(&e.to_string()) => {
            sp.fail(&e.to_string());
            std::process::exit(2)
        }
        Err(e) => {
            sp.fail(&format!("failed to publish {name}"));
            Err(e)
        }
    }
}

// The registry's words for a version it already holds.
fn already_published(error: &str) -> bool {
    error.ends_with("is already published.")
}

/* A JavaScript file the bundle carries, which no host would load, so it is never published. */
fn javascript_in(artifact: &[u8]) -> Option<String> {
    let files = crate::pack::Bundle::decode(artifact).map(|b| b.files).unwrap_or_default();
    files.into_iter().map(|f| f.path).find(|path| matches!(path.rsplit('.').next(), Some("js" | "mjs")))
}

/* What the registry made of the artifact, which is where the name and version come from now that it reads them itself. */
struct Published {
    name: String,
    version: String,
    url: String
}

/* The artifact as it sits on disk is the whole body. */
fn send(token: &str, artifact: &[u8]) -> Result<Published> {
    // A refusal still carries a body, the registry's own words for what went wrong.
    let mut response = ureq::post(site("/api/publish").as_str())
        .config()
        .http_status_as_error(false)
        .build()
        .header("authorization", &format!("Bearer {token}"))
        .header("content-type", "application/octet-stream")
        .send(artifact)
        .map_err(|e| anyhow!("reaching the registry: {e}"))?;

    let status = response.status().as_u16();
    let text = response.body_mut().read_to_string().context("reading the registry's answer")?;
    let answer: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();

    let field = |key: &str| answer.get(key).and_then(|v| v.as_str()).map(str::to_string);

    match (field("name"), field("version"), field("url")) {
        (Some(name), Some(version), Some(url)) if status < 300 => Ok(Published { name, version, url }),
        _ => bail!("{}", field("error").unwrap_or_else(|| format!("the registry refused it with {status}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{Bundle, Entry};

    #[test]
    fn a_bundle_carrying_javascript_is_caught_before_it_is_sent() {
        let packed = |path: &str| Bundle { entry: "main.py".to_string(), files: vec![Entry { path: "main.py".to_string(), bytes: b"print(1)".to_vec() }, Entry { path: path.to_string(), bytes: Vec::new() }] }.encode();
        assert_eq!(javascript_in(&packed("lib/chart.js")).as_deref(), Some("lib/chart.js"));
        assert_eq!(javascript_in(&packed("https://x/y.mjs")).as_deref(), Some("https://x/y.mjs"));
        assert_eq!(javascript_in(&packed("util.py")), None);
        assert_eq!(javascript_in(b"not a bundle"), None);
    }

    #[test]
    fn only_a_version_already_published_takes_exit_code_2() {
        assert!(already_published("greet 0.1.0 is already published."));
        assert!(!already_published("The name greet belongs to someone else."));
    }
}
