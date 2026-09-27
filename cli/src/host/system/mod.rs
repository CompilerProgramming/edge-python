mod engine;
mod pipe;

use super::Completion;
use compiler::abi::WireValue;
use serde_json::{json, Map, Value};
use std::ffi::CStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::OnceLock;

/* What a system call answered at once, a value, or that its answer arrives later as a completion. */
pub enum Called {
    Value(WireValue),
    Pending,
}

/* One request to the thread SpiderMonkey runs on. */
enum Msg {
    // A bridge function of `web.js` called with one JSON string, a call that may settle later names where it reports.
    Bridge { function: &'static CStr, json: String, reply: Sender<String>, settle: Option<(u64, u32, Sender<Completion>)> },
    // A pipe operation finished, its answer settles the promise waiting on its token.
    Io(String),
}

static THREAD: OnceLock<Sender<Msg>> = OnceLock::new();

fn thread() -> &'static Sender<Msg> {
    THREAD.get_or_init(engine::spawn)
}

fn bridge(function: &'static CStr, json: Value, settle: Option<(u64, u32, Sender<Completion>)>) -> Value {
    let (reply, answer) = channel();
    let sent = thread().send(Msg::Bridge { function, json: json.to_string(), reply, settle });
    let text = sent.ok().and_then(|_| answer.recv().ok()).unwrap_or_default();
    serde_json::from_str(&text).unwrap_or(Value::Null)
}

/* A fresh key for one run's opened modules, each interpreter instance gets its own. */
pub fn run_id() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/* The system modules, as `js/src/system/index.ts` lists them. */
pub fn modules() -> Vec<String> {
    serde_json::from_value(bridge(c"__edge_modules", Value::Null, None)).unwrap_or_default()
}

/* Why a permissions section is malformed, None when every entry is one a system module has. */
pub fn check(permissions: &Value) -> Option<String> {
    bridge(c"__edge_check", permissions.clone(), None)["error"].as_str().map(str::to_string)
}

/* The scopes `pkg` holds of `module`, None when the root grants it nothing of it. */
pub fn scopes(permissions: &Value, pkg: &str, module: &str) -> Option<Vec<String>> {
    serde_json::from_value(bridge(c"__edge_scopes", json!({ "permissions": permissions, "pkg": pkg, "module": module }), None)).unwrap_or(None)
}

/* What `pkg` asks for in its own permissions section that the root does not grant it. */
pub fn unmet(permissions: &Value, pkg: &str, section: &Value) -> Vec<String> {
    serde_json::from_value(bridge(c"__edge_unmet", json!({ "permissions": permissions, "pkg": pkg, "section": section }), None)).unwrap_or_default()
}

fn key(run: u64, pkg: &str, module: &str) -> String {
    format!("{run}:{pkg}\u{0}{module}")
}

/* Opens `module` for `pkg` in one run with the scopes it holds, the names of its calls back. */
pub fn open(run: u64, pkg: &str, module: &str, held: &[String]) -> Vec<String> {
    serde_json::from_value(bridge(c"__edge_open", json!({ "key": key(run, pkg, module), "module": module, "pkg": pkg, "held": held }), None)).unwrap_or_default()
}

/* One system call, answered now or, once its promise settles, as a completion on `events`. */
pub fn invoke(run: u64, pkg: &str, module: &str, name: &str, args: &[WireValue], call: u32, events: Sender<Completion>) -> Result<Called, String> {
    static SETTLES: AtomicU64 = AtomicU64::new(1);
    let settle = SETTLES.fetch_add(1, Ordering::Relaxed);
    let request = json!({ "key": key(run, pkg, module), "name": name, "args": args.iter().map(to_json).collect::<Vec<_>>(), "call": settle });
    let answer = bridge(c"__edge_invoke", request, Some((settle, call, events)));
    if let Some(error) = answer.get("error") {
        return Err(fault_text(error));
    }
    match answer.get("pending") {
        Some(_) => Ok(Called::Pending),
        None => Ok(Called::Value(from_json(&answer["value"]))),
    }
}

/* Aborts every request and socket a finished run left open, a process that never started the thread has none. */
pub fn close(run: u64) {
    if THREAD.get().is_some() {
        bridge(c"__edge_close", json!({ "run": run }), None);
    }
}

/* The Python exception a failed call raises, its own class for the ones a system call throws. */
fn fault_text(error: &Value) -> String {
    let (name, message) = (error[0].as_str().unwrap_or("Error"), error[1].as_str().unwrap_or(""));
    match name {
        "OSError" | "PermissionError" | "TimeoutError" | "ValueError" | "TypeError" => format!("{name}: {message}"),
        _ => format!("RuntimeError: {message}"),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len() / 2).filter_map(|i| u8::from_str_radix(text.get(i * 2..i * 2 + 2)?, 16).ok()).collect()
}

// Past this a JavaScript number loses integer precision, so a larger int crosses as text.
const SAFE_INT: i128 = (1 << 53) - 1;

/* A value as the JSON `web.js` reads, a big int, bytes and a non-finite float as one-key objects. */
fn to_json(value: &WireValue) -> Value {
    match value {
        WireValue::None => Value::Null,
        WireValue::Bool(b) => json!(b),
        WireValue::Int(i) if (-SAFE_INT..=SAFE_INT).contains(i) => json!(*i as i64),
        WireValue::Int(i) => json!({ "$int": i.to_string() }),
        WireValue::Float(f) if f.is_finite() => json!(f),
        WireValue::Float(f) => json!({ "$float": if f.is_nan() { "NaN".to_string() } else if *f > 0.0 { "Infinity".to_string() } else { "-Infinity".to_string() } }),
        WireValue::Bytes(b) => json!(String::from_utf8_lossy(b)),
        WireValue::Raw(b) => json!({ "$bytes": hex(b) }),
        WireValue::List(items) => Value::Array(items.iter().map(to_json).collect()),
        WireValue::Dict(pairs) => {
            let map: Map<String, Value> = pairs.iter().map(|(k, v)| (match k {
                WireValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
                other => to_json(other).to_string(),
            }, to_json(v))).collect();
            Value::Object(map)
        }
    }
}

/* A value `web.js` wrote, back as a value the engine takes. */
fn from_json(value: &Value) -> WireValue {
    match value {
        Value::Null => WireValue::None,
        Value::Bool(b) => WireValue::Bool(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => WireValue::Int(i as i128),
            None => WireValue::Float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => WireValue::Bytes(s.as_bytes().to_vec()),
        Value::Array(items) => WireValue::List(items.iter().map(from_json).collect()),
        Value::Object(map) if map.len() == 1 => match map.iter().next() {
            Some((k, Value::String(s))) if k == "$int" => s.parse().map(WireValue::Int).unwrap_or(WireValue::None),
            Some((k, Value::String(s))) if k == "$bytes" => WireValue::Raw(unhex(s)),
            Some((k, Value::String(s))) if k == "$float" => WireValue::Float(s.parse().unwrap_or(match s.as_str() {
                "Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ => f64::NAN,
            })),
            _ => dict(map),
        },
        Value::Object(map) => dict(map),
    }
}

fn dict(map: &Map<String, Value>) -> WireValue {
    WireValue::Dict(map.iter().map(|(k, v)| (WireValue::Bytes(k.as_bytes().to_vec()), from_json(v))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    fn settled(rx: &Receiver<Completion>) -> Result<WireValue, String> {
        match rx.recv_timeout(Duration::from_secs(10)).expect("the call settled") {
            Completion::Value { value, .. } => Ok(value),
            Completion::Error { msg, .. } => Err(msg),
            Completion::Event(e) => panic!("unexpected event {e}"),
        }
    }

    fn answered(run: u64, pkg: &str, module: &str, name: &str, args: &[WireValue]) -> Result<WireValue, String> {
        let (events, rx) = channel();
        match invoke(run, pkg, module, name, args, 0, events)? {
            Called::Value(value) => Ok(value),
            Called::Pending => settled(&rx),
        }
    }

    fn text(s: &str) -> WireValue {
        WireValue::Bytes(s.as_bytes().to_vec())
    }

    #[test]
    fn the_modules_and_their_grants_come_from_the_system_calls() {
        assert_eq!(modules(), ["net", "time"]);
        let permissions = json!({ "all": ["time:wall"], "main": ["net:api.example.com"], "http": ["net"] });
        assert_eq!(scopes(&permissions, "main", "time"), Some(vec!["wall".to_string()]));
        assert_eq!(scopes(&permissions, "http", "net"), Some(vec![]));
        assert_eq!(scopes(&permissions, "analytics", "net"), None);
        assert_eq!(check(&permissions), None);
        assert_eq!(check(&json!({ "main": ["fs:/"] })).as_deref(), Some("permissions for 'main' name 'fs', which is not a system module (net, time)"));
        let asks = json!({ "main": ["net", "time:wall"], "all": ["time:zone"], "other": ["net:evil.example"] });
        assert_eq!(unmet(&permissions, "http", &asks), ["time:zone"]);
    }

    #[test]
    fn time_answers_only_the_clocks_a_package_holds() {
        let run = run_id();
        assert_eq!(open(run, "main", "time", &["wall".to_string()]), ["now", "zone"]);
        assert!(matches!(answered(run, "main", "time", "now", &[]), Ok(WireValue::Int(ns)) if ns > 1_700_000_000_000_000_000));
        let denied = answered(run, "main", "time", "now", &[text("monotonic")]).unwrap_err();
        assert_eq!(denied, "PermissionError: 'main' has no time:monotonic, edge.json grants it time:wall");
        close(run);
    }

    #[test]
    fn net_streams_a_body_and_refuses_an_ungranted_host() {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        std::thread::spawn(move || {
            let request = server.recv().unwrap();
            let body = format!("got {}", request.url());
            let _ = request.respond(tiny_http::Response::from_string(body).with_header("x-probe: 1".parse::<tiny_http::Header>().unwrap()));
        });
        let run = run_id();
        open(run, "main", "net", &["127.0.0.1".to_string()]);
        let denied = answered(run, "main", "net", "request", &[text("GET"), text("http://evil.example/")]).unwrap_err();
        assert_eq!(denied, "PermissionError: 'main' has no net:evil.example, edge.json grants it net:127.0.0.1");
        let id = answered(run, "main", "net", "request", &[text("GET"), text(&format!("http://127.0.0.1:{port}/items"))]).unwrap();
        let WireValue::List(head) = answered(run, "main", "net", "response", std::slice::from_ref(&id)).unwrap() else { panic!("a head") };
        assert_eq!(head[0], WireValue::Int(200));
        assert_eq!(answered(run, "main", "net", "read", std::slice::from_ref(&id)).unwrap(), WireValue::Raw(b"got /items".to_vec()));
        assert_eq!(answered(run, "main", "net", "read", std::slice::from_ref(&id)).unwrap(), WireValue::None);
        close(run);
    }

    #[test]
    fn net_holds_a_socket_until_it_closes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let heard = socket.read().unwrap();
            socket.send(Message::text(format!("echo {}", heard.to_text().unwrap()))).unwrap();
            socket.send(Message::binary(vec![1u8, 2])).unwrap();
            let _ = socket.close(None);
            while socket.read().is_ok() {}
        });
        let run = run_id();
        open(run, "main", "net", &["127.0.0.1".to_string()]);
        let id = answered(run, "main", "net", "connect", &[text(&format!("ws://127.0.0.1:{port}/"))]).unwrap();
        answered(run, "main", "net", "send", &[id.clone(), text("ping")]).unwrap();
        assert_eq!(answered(run, "main", "net", "read", std::slice::from_ref(&id)).unwrap(), text("echo ping"));
        assert_eq!(answered(run, "main", "net", "read", std::slice::from_ref(&id)).unwrap(), WireValue::Raw(vec![1, 2]));
        assert_eq!(answered(run, "main", "net", "read", std::slice::from_ref(&id)).unwrap(), WireValue::None);
        close(run);
    }

    use tungstenite::Message;
}
