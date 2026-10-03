use serde_json::Value;
use std::path::{Path, PathBuf};

// One past the most files fs lists, so a folder over its cap still reads as over it from the index.
pub const INDEX_LIMIT: usize = 10_001;

/* The folders fs grants anyone in a permissions section, each as a path from the root edge.json, '' for the root. */
pub fn granted(permissions: Option<&Value>) -> Vec<String> {
    let entries = permissions.and_then(Value::as_object).into_iter().flat_map(|holders| holders.values()).filter_map(Value::as_array).flatten();
    let mut dirs: Vec<String> = entries.filter_map(|entry| entry.as_str()?.strip_prefix("fs:")).map(|dir| dir.trim_start_matches('.').trim_start_matches('/').to_string()).collect();
    dirs.sort();
    dirs.dedup();
    dirs
}

/* Where `path` sits under `root`, only when no link on the way moves it, so the real file is the one the path names. */
fn located(root: &Path, path: &str) -> Option<PathBuf> {
    let written = if path.is_empty() { root.to_path_buf() } else { root.join(path) };
    (written.canonicalize().ok()? == written).then_some(written)
}

/* The text of one file under `root`, `root` already canonical, failing with the word fs names for each reason. */
pub fn read(root: &Path, path: &str, limit: u64) -> Result<String, &'static str> {
    let file = located(root, path).filter(|file| file.is_file()).ok_or("missing")?;
    if std::fs::metadata(&file).map_err(|_| "missing")?.len() > limit {
        return Err("large");
    }
    String::from_utf8(std::fs::read(&file).map_err(|_| "missing")?).map_err(|_| "binary")
}

/* Every plain file under `dir`, no link followed and no name starting with a dot, at most `limit` of them. */
pub fn list(root: &Path, dir: &str, limit: usize) -> Result<Vec<String>, &'static str> {
    let start = located(root, dir).filter(|start| start.is_dir()).ok_or("missing")?;
    let mut found = Vec::new();
    let mut pending = vec![(start, dir.to_string())];
    while let Some((at, rel)) = pending.pop() {
        for entry in std::fs::read_dir(&at).map_err(|_| "missing")?.flatten() {
            let (Ok(kind), Ok(name)) = (entry.file_type(), entry.file_name().into_string()) else { continue };
            if name.starts_with('.') || name.contains('\\') {
                continue;
            }
            let path = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if kind.is_dir() {
                pending.push((entry.path(), path));
            } else if kind.is_file() {
                found.push(path);
                if found.len() >= limit {
                    return Ok(found);
                }
            }
        }
    }
    Ok(found)
}

/* The files of every folder fs grants under `root`, the list a page reads since http cannot list a folder. */
pub fn index(root: &Path, permissions: Option<&Value>, limit: usize) -> Vec<String> {
    let Ok(root) = root.canonicalize() else { return Vec::new() };
    let mut all: Vec<String> = granted(permissions).iter().flat_map(|dir| list(&root, dir, limit).unwrap_or_default()).collect();
    all.sort();
    all.dedup();
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("edge-files-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(dir.join("shop/app")).unwrap();
        fs::write(dir.join("shop/Gemfile"), "gem 'rails'\n").unwrap();
        fs::write(dir.join("shop/app/product.rb"), "class Product; end\n").unwrap();
        fs::write(dir.join("shop/.env"), "KEY=1\n").unwrap();
        fs::write(dir.join("shop/logo.png"), [0xff, 0xd8, 0xff]).unwrap();
        fs::write(dir.join("secret.txt"), "outside\n").unwrap();
        dir.canonicalize().unwrap()
    }

    #[test]
    fn a_file_reads_as_text_and_every_other_answer_names_its_reason() {
        let root = project();
        assert_eq!(read(&root, "shop/Gemfile", 1 << 20).unwrap(), "gem 'rails'\n");
        assert_eq!(read(&root, "shop/nope.rb", 1 << 20), Err("missing"));
        assert_eq!(read(&root, "shop/app", 1 << 20), Err("missing"));
        assert_eq!(read(&root, "shop/logo.png", 1 << 20), Err("binary"));
        assert_eq!(read(&root, "shop/Gemfile", 4), Err("large"));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_never_reads_even_one_pointing_inside() {
        let root = project();
        std::os::unix::fs::symlink(root.join("secret.txt"), root.join("shop/out.txt")).unwrap();
        std::os::unix::fs::symlink(root.join("shop/Gemfile"), root.join("shop/in.txt")).unwrap();
        std::os::unix::fs::symlink(&root, root.join("shop/up")).unwrap();
        assert_eq!(read(&root, "shop/out.txt", 1 << 20), Err("missing"));
        assert_eq!(read(&root, "shop/in.txt", 1 << 20), Err("missing"));
        assert_eq!(read(&root, "shop/up/secret.txt", 1 << 20), Err("missing"));
        assert_eq!(list(&root, "shop", 100).map(|mut found| { found.sort(); found }), Ok(vec!["shop/Gemfile".to_string(), "shop/app/product.rb".to_string(), "shop/logo.png".to_string()]));
    }

    #[test]
    fn a_list_skips_hidden_names_and_stops_at_its_limit() {
        let root = project();
        let mut found = list(&root, "shop", 100).unwrap();
        found.sort();
        assert_eq!(found, ["shop/Gemfile", "shop/app/product.rb", "shop/logo.png"]);
        assert_eq!(list(&root, "shop", 2).unwrap().len(), 2);
        assert_eq!(list(&root, "nope", 100), Err("missing"));
        let permissions = serde_json::json!({ "main": ["fs:./shop/app", "net:a.test"], "lint": ["fs:./shop/app"] });
        assert_eq!(index(&root, Some(&permissions), 100), ["shop/app/product.rb"]);
    }
}
