use super::{cache_root, cdn, get, js, plugins, system, Instance, ORIGIN};
use crate::lock::{self, Lock};
use compiler::modules::{dir_of, join_relative, parse_integrity, parse_manifest, scan_imports, system_spec, walk_up_dirs, ImportSpec};
use compiler::util::sha256::{hex_encode, sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read;
use std::path::PathBuf;
use std::rc::Rc;

// How a fetch error reads when the server says the file does not exist.
const ABSENT: &str = "not found on the server";
// Bounds a runaway download, the largest module is well under a megabyte.
const MAX_FETCH_BYTES: u64 = 64 << 20;

/* Where a run's files come from and which bare names it may resolve. */
#[derive(Clone, Default)]
pub struct Project {
    pub entry_dir: String,
    pub manifest: Option<String>,
    // An in-memory tree replaces the disk, untrusted runs always carry one.
    pub bundle: Option<Rc<HashMap<String, Vec<u8>>>>,
    pub untrusted: bool,
}

impl Project {
    pub fn disk(entry_dir: &str, manifest: Option<&str>) -> Project {
        Project { entry_dir: entry_dir.to_string(), manifest: manifest.map(String::from), bundle: None, untrusted: false }
    }

    pub fn bundle(files: HashMap<String, Vec<u8>>, entry_dir: &str, untrusted: bool) -> Project {
        Project { entry_dir: entry_dir.to_string(), manifest: None, bundle: Some(Rc::new(files)), untrusted }
    }
}

/* Registers every module `root_src` reaches, mirroring the lazy prefetch of the JS host. */
pub fn prefetch(inst: &mut Instance, root_src: &str) -> Result<(), String> {
    Walk::new(inst).run(root_src)
}

struct Walk<'a> {
    inst: &'a mut Instance,
    project: Project,
    // Bare name to spec, the nearest manifest wins.
    table: HashMap<String, String>,
    visited: HashSet<String>,
    queue: VecDeque<String>,
    failures: Vec<String>,
    // Bare names seen before a manifest declared them, retried after each merge.
    pending_bare: Vec<(String, Option<String>)>,
    // Root-relative imports waiting on their importer's manifest chain.
    pending_root: Vec<(String, String, Option<String>)>,
    manifest_dirs: HashSet<String>,
    missing: HashSet<String>,
    // Spec to the name its first importer wrote and that importer's own name, None for the entry.
    origins: HashMap<String, (String, Option<String>)>,
    // Files of every imported package, keyed under the package spec it came in as.
    mounted: HashMap<String, Vec<u8>>,
    // Every manifest read, by its dir, for the package name and the permissions it carries.
    manifests: HashMap<String, serde_json::Value>,
}

impl<'a> Walk<'a> {
    fn new(inst: &'a mut Instance) -> Self {
        let project = inst.project.clone();
        Walk {
            inst,
            project,
            table: HashMap::new(),
            visited: HashSet::new(),
            queue: VecDeque::new(),
            failures: Vec::new(),
            pending_bare: Vec::new(),
            pending_root: Vec::new(),
            manifest_dirs: HashSet::new(),
            missing: HashSet::new(),
            origins: HashMap::new(),
            mounted: HashMap::new(),
            manifests: HashMap::new(),
        }
    }

    fn run(mut self, root_src: &str) -> Result<(), String> {
        let entry_dir = self.project.entry_dir.clone();
        for imp in scan_imports(root_src) {
            self.enqueue_import(imp, &entry_dir, None);
        }
        self.enqueue_manifest_chain(&entry_dir);
        while let Some(spec) = self.queue.pop_front() {
            if !self.visited.insert(spec.clone()) {
                continue;
            }
            if spec.ends_with("edge.json") {
                self.manifest(&spec);
                continue;
            }
            match extension(&spec) {
                "edge" => self.package(&spec),
                "js" | "mjs" => self.javascript(&spec),
                "so" | "dylib" => self.refuse(&spec, "is not supported, ship a .wasm"),
                ext => match self.fetch(&spec) {
                    // A .wasm spec is a plugin, past .py the wasm magic marks one too.
                    Ok(Some(bytes)) if ext == "wasm" || (ext != "py" && bytes.starts_with(b"\0asm")) => self.plugin(&spec, &bytes),
                    Ok(Some(bytes)) => self.module(&spec, bytes),
                    Ok(None) => self.failures.push(format!("could not read module '{}'", target(&spec))),
                    Err(e) => self.failures.push(e),
                },
            }
        }
        self.serve_system();
        self.refuse_undeclared();
        if self.failures.is_empty() {
            return Ok(());
        }
        Err(self.failures.iter().map(|f| format!("error: {f}")).collect::<Vec<_>>().join("\n"))
    }

    /* Serves the system modules to every package the walk met, opened with its scopes or refused when the root grants it none. */
    fn serve_system(&mut self) {
        let root = match self.root_for(&self.project.entry_dir) {
            Some(Some(root)) => norm(&root).to_string(),
            _ => String::new(),
        };
        // An untrusted run holds no permission, whatever its manifest says.
        let declared = match self.project.untrusted {
            true => None,
            false => self.manifests.get(&root).and_then(|m| m.get("permissions")).cloned(),
        };
        if let Some(problem) = declared.as_ref().and_then(system::check) {
            self.failures.push(format!("edge.json at '{root}edge.json': {problem}"));
            return;
        }
        let permissions = declared.unwrap_or_else(|| serde_json::json!({}));
        // A run that reads no clock sleeps on the virtual one, so what it prints never depends on when it runs.
        let clock = permissions.as_object().into_iter().flat_map(|holders| holders.values()).filter_map(|entries| entries.as_array()).flatten().any(|e| e.as_str().is_some_and(|e| e.starts_with("time:")));
        if let Err(e) = self.inst.set_wall_clock(clock) {
            self.failures.push(e);
        }
        // SpiderMonkey only starts for a program that imports a name no manifest declares, the only way to reach a system module.
        if self.pending_bare.is_empty() {
            return;
        }
        let modules = system::modules();
        let mut dirs: Vec<String> = self.manifest_dirs.iter().cloned().collect();
        dirs.sort();
        for dir in dirs {
            let pkg = self.package_of(&dir, &root);
            for module in &modules {
                let spec = system_spec(module, &dir);
                let served = match system::scopes(&permissions, &pkg, module) {
                    Some(held) => self.inst.register_system(&spec, &pkg, module, &held),
                    None => self.inst.register_error(&spec, &format!("'{pkg}' imports {module}, which edge.json does not grant it")),
                };
                if let Err(e) = served {
                    self.failures.push(e);
                }
            }
        }
    }

    /* The package a manifest dir belongs to, `main` for the root, else the name its manifest declares or the dir itself. */
    fn package_of(&self, dir: &str, root: &str) -> String {
        let dir = norm(dir);
        if dir == root {
            return "main".to_string();
        }
        // A package named all or main answers to its dir, so it never takes their grants.
        let name = self.manifests.get(dir).and_then(|m| m.get("name")).and_then(|n| n.as_str()).filter(|n| !crate::manifest::RESERVED.contains(n));
        name.map_or_else(|| dir.to_string(), str::to_string)
    }

    /* A bare name no manifest declared fails at its import, with the command that declares it. */
    fn refuse_undeclared(&mut self) {
        let names: HashSet<String> = self.pending_bare.drain(..).map(|(name, _)| name).collect();
        for name in names {
            // 010100101010 ONCE EDGE-PYTHON-STD PUBLISHES TO THE REGISTRY, SUGGEST `edge add <name>` FOR A NAME THE REGISTRY KNOWS.
            let msg = format!("module '{name}' is not provided by this host and no edge.json declares it\nhelp: declare it in edge.json, or use a relative import");
            if let Err(e) = self.inst.register_error(&name, &msg) {
                self.failures.push(e);
            }
        }
    }

    // A code module registers, then its own imports queue so transitive deps stay lazy.
    fn module(&mut self, spec: &str, bytes: Vec<u8>) {
        if let Err(e) = self.inst.register_code(spec, &bytes) {
            self.failures.push(e);
            return;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.inst.store.data_mut().fetched.insert(spec.to_string(), bytes);
        let dir = dir_of(spec);
        let via = self.origins.get(spec).map(|(name, _)| name.clone());
        for imp in scan_imports(&text) {
            self.enqueue_import(imp, &dir, via.as_deref());
        }
        self.enqueue_manifest_chain(&dir);
    }

    /* A published package, verified whole, then its entry runs as the module and its other files answer from inside it. */
    fn package(&mut self, spec: &str) {
        let bytes = match self.fetch(spec) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return self.failures.push(format!("could not read package '{}'", target(spec))),
            Err(e) => return self.failures.push(e),
        };
        let bundle = match crate::pack::Bundle::decode(&bytes) {
            Ok(bundle) => bundle,
            Err(e) => return self.failures.push(format!("package '{}' is not a packed .edge, {e}", target(spec))),
        };
        let base = dir_of(spec);
        let entry = format!("{base}{}", bundle.entry);
        for file in bundle.files {
            self.mounted.insert(format!("{base}{}", file.path), file.bytes);
        }
        match self.mounted.get(&entry).cloned() {
            Some(code) => self.module(spec, code),
            None => self.failures.push(format!("package '{}' names an entry it does not carry", target(spec))),
        }
    }

    /* A third party wasm plugin, compiled by Cranelift and instantiated beside the compiler. */
    fn plugin(&mut self, spec: &str, bytes: &[u8]) {
        if self.project.untrusted {
            self.refuse(spec, "is not available to untrusted eval runs");
            return;
        }
        let name = self.origins.get(spec).map_or_else(|| target(spec).to_string(), |(name, _)| name.clone());
        if let Err(e) = plugins::register_bytes(self.inst, &name, spec, bytes) {
            self.failures.push(e);
        }
    }

    /* A JavaScript module and the files its relative imports reach, run by the JavaScript runtime. */
    fn javascript(&mut self, spec: &str) {
        let (name, via) = self.origins.get(spec).cloned().unwrap_or_else(|| (target(spec).to_string(), None));
        let registered = match self.js_tree(spec, &name) {
            Ok((entry, tree)) => self.inst.register_js(spec, &name, entry, tree),
            Err(e) => Err(e),
        };
        if let Err(msg) = registered {
            let via = via.map(|v| format!(" (via {v})")).unwrap_or_default();
            if let Err(e) = self.inst.register_error(spec, &format!("{msg}{via}")) {
                self.failures.push(e);
            }
        }
    }

    /* The entry path inside the tree plus every file, each fetched beside the entry. */
    fn js_tree(&mut self, spec: &str, name: &str) -> Result<(String, js::Tree), String> {
        let url = target(spec);
        let (base, entry) = match url.rsplit_once('/') {
            Some((base, entry)) => (format!("{base}/"), entry.to_string()),
            None => (String::new(), url.to_string()),
        };
        let first = self.fetch(spec)?.ok_or_else(|| format!("could not read module '{url}'"))?;
        let mut queue = vec![(entry.clone(), Some(first))];
        let mut seen = HashSet::new();
        let mut tree = Vec::new();
        while let Some((rel, bytes)) = queue.pop() {
            if !seen.insert(rel.clone()) {
                continue;
            }
            let bytes = match bytes {
                Some(bytes) => bytes,
                None => self.fetch(&format!("{base}{rel}"))?.ok_or_else(|| format!("could not read module '{base}{rel}'"))?,
            };
            for dep in js::imports(&String::from_utf8_lossy(&bytes)) {
                let clean = dep.split(['?', '#']).next().unwrap_or(dep);
                let file = js::join(&rel, clean).ok_or_else(|| format!("module '{name}' imports '{dep}' from outside its directory"))?;
                queue.push((file, None));
            }
            tree.push((rel, bytes));
        }
        Ok((entry, tree))
    }

    /* Why a module cannot load here, raised at its import and named as its importer wrote it. */
    fn refuse(&mut self, spec: &str, reason: &str) {
        let (name, via) = self.origins.get(spec).cloned().unwrap_or_else(|| (target(spec).to_string(), None));
        let via = via.map(|v| format!(" (via {v})")).unwrap_or_default();
        if let Err(e) = self.inst.register_error(spec, &format!("module '{name}' {reason}{via}")) {
            self.failures.push(e);
        }
    }

    /* Merges a manifest into the table and serves it to the compiler as written. */
    fn manifest(&mut self, spec: &str) {
        let bytes = match self.read_manifest(spec) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                self.missing.insert(spec.to_string());
                self.retry_root();
                return;
            }
            Err(e) => {
                self.failures.push(e);
                return;
            }
        };
        let parsed = match parse_manifest(&bytes) {
            Ok(m) => m,
            Err(e) => {
                self.failures.push(format!("edge.json at '{spec}': {e}"));
                return;
            }
        };
        // A package says the lowest engine it runs on, so one written for a later one is refused where it loads.
        if let Some(floor) = crate::manifest::floor(&bytes)
            && crate::manifest::newer(&floor)
        {
            self.failures.push(format!("edge.json at '{spec}': needs edge {floor}, this is {}", env!("CARGO_PKG_VERSION")));
            return;
        }
        let dir = dir_of(spec);
        self.manifest_dirs.insert(dir.clone());
        if let Ok(value) = serde_json::from_slice(&bytes) {
            self.manifests.insert(norm(&dir).to_string(), value);
        }
        // Every version it declares becomes the url and digest its lock holds, before a name or the compiler sees it.
        let resolved = match self.resolve_versions(spec, &parsed.imports) {
            Ok(resolved) => resolved,
            Err(e) => {
                self.failures.push(format!("edge.json at '{spec}': {e}"));
                return;
            }
        };
        let rewritten = resolved.iter().zip(&parsed.imports).any(|((_, target), (_, declared))| target != declared);
        for (name, target) in &resolved {
            self.table.entry(name.clone()).or_insert_with(|| join_relative(&dir, target));
        }
        let served = match rewritten {
            true => serve_manifest(&resolved, parsed.extends.as_deref()),
            false => bytes,
        };
        self.inst.store.data_mut().fetched.insert(spec.to_string(), served);
        self.retry_pending();
        self.retry_root();
        if let Some(ext) = &parsed.extends {
            let mut next = join_relative(&dir, ext);
            if !next.ends_with('/') {
                next.push('/');
            }
            self.queue.push_back(format!("{next}edge.json"));
        }
    }

    /* Each declared target as the lock beside this manifest resolved it, so a run reads where a name points rather than asking the registry. */
    fn resolve_versions(&mut self, spec: &str, imports: &[(String, String)]) -> Result<Vec<(String, String)>, String> {
        if !imports.iter().any(|(_, target)| needs_lock(target)) {
            return Ok(imports.to_vec());
        }
        let lock = match self.read_beside(spec, lock::FILE)? {
            Some(bytes) => Lock::parse(&bytes).map_err(|e| format!("{e:#}"))?,
            None => Lock::default(),
        };
        imports.iter().map(|(name, target)| Ok((name.clone(), lock.spec(name, target).map_err(|e| format!("{e:#}"))?))).collect()
    }

    /* A file beside a manifest, read the way that manifest was, so a lock follows its project onto disk, into a bundle or behind a url. */
    fn read_beside(&mut self, manifest: &str, name: &str) -> Result<Option<Vec<u8>>, String> {
        if manifest == "edge.json"
            && let Some(path) = self.project.manifest.clone()
        {
            let beside = std::path::Path::new(&path).with_file_name(name);
            return match std::fs::read(&beside) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(format!("reading {}: {e}", beside.display())),
            };
        }
        Ok(self.read_sibling(&format!("{}{name}", dir_of(manifest))))
    }

    /* Bytes for a file a manifest sits beside, from the package that carries it, the bundle, or the network, in that order. A packed package answers for its own files, so one fetched from a url still reads the manifest inside it. */
    fn read_sibling(&mut self, spec: &str) -> Option<Vec<u8>> {
        let remote = spec.contains("://") && self.project.bundle.is_none() && !self.mounted.contains_key(spec);
        match remote {
            true => fetch_manifest(spec),
            false => self.fetch(spec).unwrap_or(None),
        }
    }

    /* The root manifest always exists, a `--manifest` override is then the only manifest. */
    fn read_manifest(&mut self, spec: &str) -> Result<Option<Vec<u8>>, String> {
        let root = spec == "edge.json";
        if let Some(path) = self.project.manifest.clone() {
            if !root {
                return Ok(None);
            }
            return match std::fs::read(&path) {
                Ok(bytes) => Ok(Some(bytes)),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Some(b"{}".to_vec())),
                Err(e) => Err(format!("reading {path}: {e}")),
            };
        }
        // A remote manifest that answered 404 once stays absent, so later runs skip the request.
        let bytes = self.read_sibling(spec);
        if root && bytes.is_none() {
            return Ok(Some(b"{}".to_vec()));
        }
        Ok(bytes)
    }

    // Queues a module spec, the first importer to reach it names it in refusals.
    fn enqueue(&mut self, spec: String, name: String, via: Option<String>) {
        self.origins.entry(spec.clone()).or_insert((name, via));
        self.queue.push_back(spec);
    }

    fn enqueue_import(&mut self, imp: ImportSpec, dir: &str, via: Option<&str>) {
        let via = via.map(String::from);
        match imp {
            ImportSpec::Relative(path) => self.enqueue(join_relative(dir, &path), path, via),
            ImportSpec::Root(path) => self.enqueue_root(path, dir.to_string(), via),
            ImportSpec::Bare(name) => match self.table.get(&name) {
                Some(spec) => self.enqueue(spec.clone(), name, via),
                None => self.pending_bare.push((name, via)),
            },
        }
    }

    fn retry_pending(&mut self) {
        let pending = std::mem::take(&mut self.pending_bare);
        for (name, via) in pending {
            match self.table.get(&name) {
                Some(spec) => self.enqueue(spec.clone(), name, via),
                None => self.pending_bare.push((name, via)),
            }
        }
    }

    // Probes every ancestor manifest, mirroring the compiler walk-up.
    fn enqueue_manifest_chain(&mut self, dir: &str) {
        let chain: Vec<String> = walk_up_dirs(dir).collect();
        for d in chain {
            let m = format!("{d}edge.json");
            if !self.missing.contains(&m) {
                self.queue.push_back(m);
            }
        }
    }

    /* Nearest manifest dir at or above `dir`, None while probes pend, Some(None) once probed bare. */
    fn root_for(&self, dir: &str) -> Option<Option<String>> {
        for d in walk_up_dirs(dir) {
            if self.manifest_dirs.contains(&d) {
                return Some(Some(d));
            }
            let m = format!("{d}edge.json");
            if !self.visited.contains(&m) && !self.missing.contains(&m) {
                return None;
            }
        }
        Some(None)
    }

    fn enqueue_root(&mut self, spec: String, dir: String, via: Option<String>) {
        match self.root_for(&dir) {
            None => self.pending_root.push((spec, dir, via)),
            Some(Some(root)) => self.enqueue(join_relative(&root, &spec), spec, via),
            // No manifest anywhere, the compiler reports it.
            Some(None) => {}
        }
    }

    fn retry_root(&mut self) {
        let pending = std::mem::take(&mut self.pending_root);
        for (spec, dir, via) in pending {
            self.enqueue_root(spec, dir, via);
        }
    }

    /* Bytes for a spec, the bundle, a pinned download or the disk, None when absent. */
    fn fetch(&mut self, spec: &str) -> Result<Option<Vec<u8>>, String> {
        let (target, pin) = parse_integrity(spec)?;
        if let Some(bytes) = self.mounted.get(target) {
            return Ok(Some(bytes.clone()));
        }
        let packed = self.project.bundle.as_ref().map(|files| files.get(target.strip_prefix("./").unwrap_or(target)).cloned());
        let bytes = if let Some(Some(bytes)) = packed {
            Some(bytes)
        } else if target.contains("://") {
            // An untrusted run reads remote modules from the official origin only.
            if self.project.untrusted && !target.starts_with(&format!("{ORIGIN}/")) {
                return Err(format!("module '{target}' is not available to untrusted eval runs"));
            }
            Some(fetch_cached(target, pin)?)
        } else if packed.is_some() {
            // Bundle paths are plain, a joined spec may still carry the importer's leading dot.
            None
        } else {
            match std::fs::read(target) {
                Ok(bytes) => Some(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(format!("cannot read module '{target}': {e}")),
            }
        };
        if let (Some(bytes), Some(pin)) = (&bytes, pin)
            && !target.contains("://")
        {
            check_pin(target, bytes, None, Some(pin))?;
        }
        Ok(bytes)
    }
}

/* A dir as one spelling, since `./lib/` and `lib/` are the same place under the project. */
fn norm(dir: &str) -> &str {
    dir.trim_start_matches("./")
}

fn target(spec: &str) -> &str {
    spec.split_once('#').map_or(spec, |(t, _)| t)
}

/* Whether a target says nothing about where its bytes are, or says it without a digest, which is what a lock answers. */
fn needs_lock(target: &str) -> bool {
    lock::version_of(target).is_some() || (target.contains("://") && !target.contains("#sha256-"))
}

/* The manifest as the compiler reads it, which knows paths and urls and never a version. */
fn serve_manifest(imports: &[(String, String)], extends: Option<&str>) -> Vec<u8> {
    let mut manifest = serde_json::Map::new();
    let table: serde_json::Map<String, serde_json::Value> =
        imports.iter().map(|(name, target)| (name.clone(), serde_json::Value::String(target.clone()))).collect();
    manifest.insert("imports".to_string(), serde_json::Value::Object(table));
    if let Some(extends) = extends {
        manifest.insert("extends".to_string(), serde_json::Value::String(extends.to_string()));
    }
    serde_json::Value::Object(manifest).to_string().into_bytes()
}

/* The extension of the last path segment, query and fragment stripped, it picks how a module loads. */
fn extension(spec: &str) -> &str {
    let path = spec.split(['?', '#']).next().unwrap_or(spec);
    let file = path.rsplit('/').next().unwrap_or(path);
    file.rsplit_once('.').map_or("", |(_, ext)| ext)
}


/* A remote manifest, None when it is absent, a 404 leaves a `.missing` marker in the cache. */
fn fetch_manifest(url: &str) -> Option<Vec<u8>> {
    let dir = cache_dir().ok()?;
    let marker = dir.join(format!("{}.missing", hex_encode(&sha256(cdn(url).as_bytes()))));
    if marker.exists() {
        return None;
    }
    match fetch_cached(url, None) {
        Ok(bytes) => Some(bytes),
        Err(e) => {
            if e.ends_with(ABSENT) {
                let _ = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&marker, b""));
            }
            None
        }
    }
}

/* Downloads once into the user cache, a `.lock` sidecar pins the digest like the JS host lockfile. */
pub fn fetch_cached(url: &str, expected: Option<[u8; 32]>) -> Result<Vec<u8>, String> {
    let dir = cache_dir()?;
    let ext = url.rsplit('.').next().unwrap_or("bin");
    // Keyed by the address actually fetched, so a staging origin never fills a production entry.
    let source = cdn(url);
    let file = dir.join(format!("{}.{ext}", hex_encode(&sha256(source.as_bytes()))));
    let lock = file.with_extension(format!("{ext}.lock"));
    if file.exists() {
        let bytes = std::fs::read(&file).map_err(|e| format!("cannot read cached '{url}': {e}"))?;
        // A stale blob under a new explicit pin is a miss, so refetch instead of failing.
        match check_pin(url, &bytes, std::fs::read_to_string(&lock).ok(), expected) {
            Ok(_) => return Ok(bytes),
            Err(e) if expected.is_none() => return Err(e),
            Err(_) => {}
        }
    }
    let mut resp = get(&source).map_err(|e| match e {
        ureq::Error::StatusCode(404 | 410) => format!("fetching '{source}': {ABSENT}"),
        e => format!("fetching '{source}': {e}"),
    })?;
    let mut bytes = Vec::new();
    resp.body_mut().as_reader().take(MAX_FETCH_BYTES).read_to_end(&mut bytes).map_err(|e| format!("reading '{source}': {e}"))?;
    let got = check_pin(url, &bytes, None, expected)?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("creating cache dir: {e}"))?;
    // Temp plus rename keeps a truncated download out of the shared cache.
    let tmp = file.with_extension(format!("{ext}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, &bytes).map_err(|e| format!("writing cache for '{url}': {e}"))?;
    std::fs::rename(&tmp, &file).map_err(|e| format!("writing cache for '{url}': {e}"))?;
    std::fs::write(&lock, &got).map_err(|e| format!("writing cache for '{url}': {e}"))?;
    Ok(bytes)
}

/* Hashes `bytes` against an explicit pin, else the sidecar record, unpinned bytes set the pin. */
fn check_pin(spec: &str, bytes: &[u8], locked: Option<String>, expected: Option<[u8; 32]>) -> Result<String, String> {
    let got = hex_encode(&sha256(bytes));
    if let Some(want) = expected.map(|h| hex_encode(&h)) {
        if want != got {
            return Err(format!("integrity check failed for '{spec}'\n expected sha256-{want}\n got sha256-{got}"));
        }
    } else if let Some(want) = locked
        && want != got
    {
        return Err(format!("integrity drift for '{spec}'\n  locked: sha256-{want}\n  remote: sha256-{got}"));
    }
    Ok(got)
}

fn cache_dir() -> Result<PathBuf, String> {
    Ok(cache_root()?.join("modules"))
}
