use anyhow::{bail, Context, Result};
use std::fs;
use std::path::Path;

const SCAFFOLD_MAIN_PY: &str = "print(\"hello from edge python\")\n";
// A new project records the engine it was written for, so a later one keeps running it and an earlier one says why it cannot.
const EDGE_JSON: &str = concat!("{\n  \"edge\": \"", env!("CARGO_PKG_VERSION"), "\"\n}\n");
const INDEX_HTML: &str = include_str!("../templates/scaffold.html");

fn index_html(title: &str) -> String {
    INDEX_HTML.replace("__EDGE_TITLE__", title)
}

/// Scaffold a ready-to-run project (entry script, host page, manifest).
pub fn run(name: Option<&str>, bare: bool) -> Result<()> {
    let dir = name.unwrap_or(".");
    let root = Path::new(dir);

    if dir != "." {
        if root.exists() {
            bail!("'{dir}' already exists");
        }
        fs::create_dir_all(root).with_context(|| format!("creating {dir}"))?;
    }

    fs::write(root.join("main.py"), SCAFFOLD_MAIN_PY)?;
    fs::write(root.join("edge.json"), EDGE_JSON)?;

    let mut items = vec![];
    if !bare {
        let title = if dir == "." { "edge app" } else { dir };
        fs::write(root.join("index.html"), index_html(title))?;
        items.push("index.html");
    }
    items.push("main.py");
    items.push("edge.json");

    let next = if dir == "." {
        "edge serve".to_string()
    } else {
        format!("cd {dir} && edge serve")
    };
    crate::ui::scaffolded(dir, &items, &next);
    Ok(())
}
