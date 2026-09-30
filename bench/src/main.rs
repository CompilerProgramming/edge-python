use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;
use wasmtime::{Config, Engine, Instance, Linker, Memory, Module, Store, TypedFunc};

const CASES: &str = include_str!("../../tests/cases/vm.json");
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");
const SNAPSHOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/.snapshot");
// Only the cases that exhaust the budget reach this many ops, and they end the same way sooner.
const OPS: i64 = 10_000_000;
// nearcore prices wasm instructions at 822756 gas and one Tgas at 1 ms, so 0.82 ns.
const SECONDS_PER_INSTRUCTION: f64 = 822_756.0 * 1e-15;
// A report names at most this many cases.
const TOP: usize = 5;

// The reference seconds each case takes, the same on every machine and every run.
#[derive(serde::Deserialize)]
struct Snapshot {
    rustc: String,
    threshold: f64,
    case_threshold: f64,
    cases: BTreeMap<String, f64>,
}

fn main() {
    let update = std::env::args().any(|a| a == "--update");
    let wasm = std::env::var_os("EDGE_COMPILER_WASM").map(PathBuf::from).unwrap_or_else(|| Path::new(ROOT).join("target/wasm32-unknown-unknown/cli/compiler.wasm"));
    let built = std::fs::metadata(&wasm).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    if built < newest(&Path::new(ROOT).join("src")) {
        println!("{} is older than src, build it with cargo wasm-cli", wasm.display());
        std::process::exit(1);
    }
    let bytes = std::fs::read(&wasm).expect("reading compiler.wasm");
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config).expect("wasmtime engine");
    let module = Module::new(&engine, &bytes).expect("compiling compiler.wasm");

    let cases: Vec<serde_json::Value> = serde_json::from_str(CASES).expect("tests/cases/vm.json is not valid JSON");
    println!("vm.json  {} cases", cases.len());
    let seconds = cases.iter().map(|case| (key(case), (run(&engine, &module, case) as f64 * SECONDS_PER_INSTRUCTION * 1e9).round() / 1e9)).collect();
    let rustc = Command::new("rustc").arg("--version").current_dir(ROOT).output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let now = Snapshot { rustc, threshold: 0.005, case_threshold: 0.05, cases: seconds };
    let sources: BTreeMap<String, &str> = cases.iter().map(|c| (key(c), c["src"].as_str().unwrap_or(""))).collect();

    let last = std::fs::read_to_string(SNAPSHOT).ok().and_then(|s| serde_json::from_str::<Snapshot>(&s).ok());
    let failures = last.as_ref().map(|l| check(l, &now, &sources)).unwrap_or_default();
    for failure in &failures {
        annotate("error", failure);
    }
    if update || last.is_none() {
        let thresholds = last.map_or((now.threshold, now.case_threshold), |l| (l.threshold, l.case_threshold));
        write(&Snapshot { threshold: thresholds.0, case_threshold: thresholds.1, ..now });
        return println!("  snapshot written");
    }
    if !failures.is_empty() {
        std::process::exit(1);
    }
}

/* Every rule a run is held to, one paragraph for each that fails, empty when the run matches the snapshot. */
fn check(last: &Snapshot, now: &Snapshot, sources: &BTreeMap<String, &str>) -> Vec<String> {
    // Another rustc compiles another compiler.wasm, whose seconds say nothing about this code.
    if last.rustc != now.rustc {
        return vec![format!("The snapshot was taken with {}, this is {}, regenerate it with {}.", last.rustc, now.rustc, last.rustc)];
    }
    let mut failures = Vec::new();
    let new = now.cases.keys().filter(|k| !last.cases.contains_key(*k)).count();
    let gone = last.cases.keys().filter(|k| !now.cases.contains_key(*k)).count();
    if new + gone > 0 {
        failures.push(format!("{new} cases are missing from the snapshot and {gone} entries have no case, run --update."));
    }

    // Every case weighs the same in a geometric mean, so no single long case decides it.
    let timed: Vec<(&String, f64, f64)> = now.cases.iter().filter_map(|(k, &n)| last.cases.get(k).filter(|&&l| l > 0.0 && n > 0.0).map(|&l| (k, l, n))).collect();
    let change = (timed.iter().map(|&(_, l, n)| (n / l).ln()).sum::<f64>() / timed.len().max(1) as f64).exp() - 1.0;
    let (total, was) = timed.iter().fold((0.0, 0.0), |(t, w), &(_, l, n)| (t + n, w + l));
    let moved = |slower: bool| {
        let mut picked: Vec<&(&String, f64, f64)> = timed.iter().filter(|(_, l, n)| if slower { n > l } else { n < l }).collect();
        picked.sort_by(|a, b| (b.2 / b.1).ln().abs().total_cmp(&(a.2 / a.1).ln().abs()));
        picked.iter().map(|&&(k, l, n)| moved_line(sources, k, l, n)).collect::<Vec<_>>()
    };
    if change > last.threshold {
        failures.push(format!("vm.json got slower, {total:.3} s against {was:.3} s, {:+.2}% across {} cases. Fix it, or accept it with --update if it is intended. Most slowed{}", change * 100.0, timed.len(), list(&moved(true))));
    } else if change < -last.threshold {
        failures.push(format!("vm.json got faster, {total:.3} s against {was:.3} s, {:+.2}%. The snapshot no longer matches the code, run --update and commit it with the change. Biggest gains{}", change * 100.0, list(&moved(false))));
    }
    let mut apart: Vec<&(&String, f64, f64)> = timed.iter().filter(|(_, l, n)| (n / l - 1.0).abs() > last.case_threshold).collect();
    apart.sort_by(|a, b| (b.2 / b.1).ln().abs().total_cmp(&(a.2 / a.1).ln().abs()));
    if !apart.is_empty() {
        let lines: Vec<String> = apart.iter().map(|&&(k, l, n)| moved_line(sources, k, l, n)).collect();
        failures.push(format!("{} cases moved past ±{:.0}% on their own.{}", apart.len(), last.case_threshold * 100.0, list(&lines)));
    }

    // Printed even when a finding fails, so adding cases never hides how the existing ones moved.
    annotate("notice", &format!("vm.json runs in {total:.3} s of reference time, {:+.2}% against the snapshot across {} cases, the band is ±{:.1}%.", change * 100.0, timed.len(), last.threshold * 100.0));
    failures
}

fn moved_line(sources: &BTreeMap<String, &str>, key: &str, last: f64, now: f64) -> String {
    format!("{} {last:.9} s to {now:.9} s ({:+.1}%)", source(sources, key), (now / last - 1.0) * 100.0)
}

fn source(sources: &BTreeMap<String, &str>, key: &str) -> String {
    format!("{:?}", sources.get(key).unwrap_or(&"").chars().take(50).collect::<String>())
}

/* The first cases of a finding, one per line, and how many more there are. */
fn list(lines: &[String]) -> String {
    let more = lines.len().saturating_sub(TOP);
    let shown = lines.iter().take(TOP).map(|l| format!("\n  {l}")).collect::<String>();
    if more > 0 { format!("{shown}\n  and {more} more.") } else { shown }
}

/* Prints a finding, and under GitHub Actions also raises it as an annotation on the run. */
fn annotate(level: &str, message: &str) {
    println!("  {message}");
    if std::env::var("GITHUB_ACTIONS").is_ok_and(|v| v == "true") {
        println!("::{level} title=Bench::{}", message.trim_end().replace('%', "%25").replace('\n', "%0A"));
    }
}

/* Runs one case on a fresh instance, feeding its input and events, and returns the instructions it executed. */
fn run(engine: &Engine, module: &Module, case: &serde_json::Value) -> u64 {
    let texts = |field: &str| case[field].as_array().map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect::<Vec<_>>()).unwrap_or_default();
    let (input, events) = (texts("input"), [texts("events"), texts("interactive_events")].concat());
    let mut store = Store::new(engine, ());
    store.set_fuel(u64::MAX).expect("fuel is enabled");
    let mut linker = Linker::new(engine);
    linker.define_unknown_imports_as_default_values(&mut store, module).expect("stubbing the host imports");
    let inst = linker.instantiate(&mut store, module).expect("instantiating compiler.wasm");
    let memory = inst.get_memory(&mut store, "memory").expect("compiler.wasm exports memory");
    func::<(i64, i64, i64), ()>(&inst, &mut store, "set_limits").call(&mut store, (0, OPS, 0)).ok();
    func::<u32, ()>(&inst, &mut store, "set_wall_clock").call(&mut store, 0).ok();
    if !input.is_empty() {
        let (ptr, len) = stage(&inst, &mut store, memory, &input.join("\n"));
        func::<(u32, u32), ()>(&inst, &mut store, "set_input").call(&mut store, (ptr, len)).ok();
    }
    let (ptr, len) = stage(&inst, &mut store, memory, case["src"].as_str().unwrap_or(""));
    let (resume, push) = (func::<(), u32>(&inst, &mut store, "run_resume"), func::<(u32, u32), i32>(&inst, &mut store, "run_push_event"));
    let mut events = events.iter();

    let before = store.get_fuel().unwrap_or(0);
    let mut status = func::<(u32, u32), u32>(&inst, &mut store, "run_start").call(&mut store, (ptr, len));
    // A timer or a preempt resumes at once, a wait for an event takes the next one, anything else ends the run.
    while let Ok(s) = status {
        status = match s >> 29 {
            1 | 7 => resume.call(&mut store, ()),
            3 => match events.next() {
                Some(event) => {
                    let (ptr, len) = stage(&inst, &mut store, memory, event);
                    push.call(&mut store, (ptr, len)).ok();
                    resume.call(&mut store, ())
                }
                None => break,
            },
            _ => break,
        };
    }
    before - store.get_fuel().unwrap_or(0)
}

fn func<P: wasmtime::WasmParams, R: wasmtime::WasmResults>(inst: &Instance, store: &mut Store<()>, name: &str) -> TypedFunc<P, R> {
    inst.get_typed_func(&mut *store, name).unwrap_or_else(|e| panic!("compiler.wasm export {name}: {e}"))
}

/* Copies `text` into the instance memory, where the exports read their arguments. */
fn stage(inst: &Instance, store: &mut Store<()>, memory: Memory, text: &str) -> (u32, u32) {
    let ptr = func::<u32, u32>(inst, store, "wasm_alloc").call(&mut *store, text.len().max(1) as u32).expect("wasm_alloc");
    memory.write(&mut *store, ptr as usize, text.as_bytes()).expect("writing into compiler.wasm memory");
    (ptr, text.len() as u32)
}

/* Writes the snapshot with every case on one line in plain decimal seconds, so each line reads without converting. */
fn write(snapshot: &Snapshot) {
    let cases: Vec<String> = snapshot.cases.iter().map(|(k, s)| format!("    \"{k}\": {s:.9}")).collect();
    let json = format!(
        "{{\n  \"rustc\": {:?},\n  \"threshold\": {},\n  \"case_threshold\": {},\n  \"cases\": {{\n{}\n  }}\n}}\n",
        snapshot.rustc, snapshot.threshold, snapshot.case_threshold, cases.join(",\n")
    );
    std::fs::write(SNAPSHOT, json).expect("writing bench/.snapshot");
}

/* The latest change under `dir`, which a compiler.wasm must not predate. */
fn newest(dir: &Path) -> SystemTime {
    std::fs::read_dir(dir).into_iter().flatten().flatten().map(|entry| {
        let path = entry.path();
        if path.is_dir() { newest(&path) } else { entry.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH) }
    }).max().unwrap_or(SystemTime::UNIX_EPOCH)
}

/* A stable key for a case, FNV-1a over every field of it so two cases sharing a source stay apart. */
fn key(case: &serde_json::Value) -> String {
    let hash = case.to_string().bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("{hash:016x}")
}
