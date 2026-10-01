use super::{from_json, to_json, Msg};
use compiler::abi::WireValue;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::Message;

// How much of a body one read hands over.
const CHUNK: usize = 64 << 10;
// How long a socket waits on a message before it looks for one to send.
const SOCKET_POLL: Duration = Duration::from_millis(20);

/* What arrived for a request or a socket and the tokens still waiting on it. */
#[derive(Default)]
struct Flow {
    head: Option<Value>,
    items: VecDeque<Value>,
    ended: bool,
    error: Option<String>,
    heads: Vec<u64>,
    reads: VecDeque<u64>,
}

type Shared = Arc<Mutex<Flow>>;

fn lock(flow: &Shared) -> MutexGuard<'_, Flow> {
    flow.lock().unwrap_or_else(|e| e.into_inner())
}

/* Settles the promise waiting on `token` with a value or an error. */
fn answer(io: &Sender<Msg>, token: u64, result: Result<Value, String>) {
    let body = match result {
        Ok(value) => json!({ "token": token, "value": value }),
        Err(error) => json!({ "token": token, "error": error }),
    };
    let _ = io.send(Msg::Io(body.to_string()));
}

/* One more item for whoever reads next, handed straight to a waiting read. */
fn push(flow: &Shared, io: &Sender<Msg>, item: Value) {
    let mut f = lock(flow);
    match f.reads.pop_front() {
        Some(token) => answer(io, token, Ok(item)),
        None => f.items.push_back(item),
    }
}

/* The end of the items, every waiting read answered with None. */
fn end(flow: &Shared, io: &Sender<Msg>) {
    let mut f = lock(flow);
    f.ended = true;
    for token in f.reads.drain(..) {
        answer(io, token, Ok(Value::Null));
    }
}

fn fail(flow: &Shared, io: &Sender<Msg>, error: String) {
    let mut f = lock(flow);
    f.error = Some(error.clone());
    let mut waiting = std::mem::take(&mut f.heads);
    waiting.extend(f.reads.drain(..));
    for token in waiting {
        answer(io, token, Err(error.clone()));
    }
}

enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

struct Request {
    flow: Shared,
    cancel: Arc<AtomicBool>,
}

struct Socket {
    flow: Shared,
    outgoing: Sender<Outgoing>,
}

/* The requests and sockets the system calls opened, each served on its own thread. */
#[derive(Default)]
pub(super) struct Pipe {
    requests: HashMap<i64, Request>,
    sockets: HashMap<i64, Socket>,
    next: i64,
}

pub(super) fn epoch() -> Instant {
    static START: OnceLock<Instant> = OnceLock::new();
    *START.get_or_init(Instant::now)
}

fn text(args: &Value, key: &str) -> Result<String, String> {
    args[key].as_str().map(str::to_string).ok_or_else(|| format!("{key} must be a str"))
}

fn bytes(value: &Value) -> Vec<u8> {
    match from_json(value) {
        WireValue::Raw(b) | WireValue::Bytes(b) => b,
        _ => Vec::new(),
    }
}

impl Pipe {
    /* One operation `web.js` asked for, answered at once, or later through the token it carries. */
    pub(super) fn handle(&mut self, op: &str, args: &Value, io: &Sender<Msg>) -> Result<Value, String> {
        let token = args["token"].as_u64().unwrap_or(0);
        let id = args["id"].as_i64().unwrap_or(-1);
        match op {
            "monotonic" => Ok(json!(epoch().elapsed().as_secs_f64() * 1000.0)),
            // Only a name the package holds asks, so the prefix keeps every other variable out of reach.
            "secret" => Ok(std::env::var(format!("EDGE_SECRET_{}", text(args, "name")?)).map_or(Value::Null, Value::String)),
            "http_start" => {
                let headers = args["headers"].as_array().into_iter().flatten().filter_map(|p| Some((p[0].as_str()?.to_string(), p[1].as_str()?.to_string()))).collect();
                let (method, url, body) = (text(args, "method")?, text(args, "url")?, bytes(&args["body"]));
                let (flow, cancel) = (Shared::default(), Arc::new(AtomicBool::new(false)));
                let (thread_flow, thread_cancel, thread_io) = (flow.clone(), cancel.clone(), io.clone());
                std::thread::spawn(move || fetch(&method, &url, headers, body, &thread_flow, &thread_cancel, &thread_io));
                self.next += 1;
                self.requests.insert(self.next, Request { flow, cancel });
                Ok(json!(self.next))
            }
            "http_head" => {
                let flow = &self.requests.get(&id).ok_or("no such request")?.flow;
                let mut f = lock(flow);
                match (&f.head, &f.error) {
                    (Some(head), _) => answer(io, token, Ok(head.clone())),
                    (None, Some(error)) => answer(io, token, Err(error.clone())),
                    (None, None) => f.heads.push(token),
                }
                Ok(Value::Null)
            }
            "http_chunk" | "ws_next" => {
                let flow = match op {
                    "http_chunk" => &self.requests.get(&id).ok_or("no such request")?.flow,
                    _ => &self.sockets.get(&id).ok_or("no such socket")?.flow,
                };
                let mut f = lock(flow);
                match f.items.pop_front() {
                    Some(item) => answer(io, token, Ok(item)),
                    None if f.ended => answer(io, token, Ok(Value::Null)),
                    None => match f.error.clone() {
                        Some(error) => answer(io, token, Err(error)),
                        None => f.reads.push_back(token),
                    },
                }
                Ok(Value::Null)
            }
            "http_abort" => {
                if let Some(request) = self.requests.remove(&id) {
                    request.cancel.store(true, Ordering::SeqCst);
                }
                Ok(Value::Null)
            }
            "ws_open" => {
                let url = text(args, "url")?;
                let (flow, (outgoing, queued)) = (Shared::default(), channel());
                self.next += 1;
                let (socket_id, thread_flow, thread_io) = (self.next, flow.clone(), io.clone());
                std::thread::spawn(move || serve_socket(&url, socket_id, token, &thread_flow, queued, &thread_io));
                self.sockets.insert(socket_id, Socket { flow, outgoing });
                Ok(Value::Null)
            }
            "ws_send" => {
                let socket = self.sockets.get(&id).ok_or("no such socket")?;
                let message = match from_json(&args["data"]) {
                    WireValue::Bytes(b) => Outgoing::Text(String::from_utf8_lossy(&b).into_owned()),
                    other => Outgoing::Binary(match other {
                        WireValue::Raw(b) => b,
                        _ => Vec::new(),
                    }),
                };
                socket.outgoing.send(message).map_err(|_| "the socket is not open".to_string())?;
                Ok(Value::Null)
            }
            "ws_close" => {
                if let Some(socket) = self.sockets.remove(&id) {
                    let _ = socket.outgoing.send(Outgoing::Close);
                }
                Ok(Value::Null)
            }
            _ => Err(format!("no pipe operation '{op}'")),
        }
    }
}

/* Runs one request to its end, handing its head and chunks to whoever waits on them. */
fn fetch(method: &str, url: &str, headers: Vec<(String, String)>, body: Vec<u8>, flow: &Shared, cancel: &AtomicBool, io: &Sender<Msg>) {
    // A redirect comes back as it is, net.ts refuses it the same way the browser does.
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).max_redirects(0).build().into();
    let mut request = ureq::http::Request::builder().method(method).uri(url);
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let response = match request.body(body).map_err(|e| e.to_string()).and_then(|r| agent.run(r).map_err(|e| e.to_string())) {
        Ok(response) => response,
        Err(e) => return fail(flow, io, e),
    };
    let pairs: Vec<Value> = response.headers().iter().map(|(k, v)| json!([k.as_str(), String::from_utf8_lossy(v.as_bytes())])).collect();
    let head = json!([response.status().as_u16(), pairs]);
    {
        let mut f = lock(flow);
        f.head = Some(head.clone());
        for token in f.heads.drain(..) {
            answer(io, token, Ok(head.clone()));
        }
    }
    let mut reader = response.into_body().into_reader();
    let mut buf = vec![0u8; CHUNK];
    while !cancel.load(Ordering::SeqCst) {
        match reader.read(&mut buf) {
            Ok(0) => return end(flow, io),
            Ok(n) => push(flow, io, to_json(&WireValue::Raw(buf[..n].to_vec()))),
            Err(e) => return fail(flow, io, e.to_string()),
        }
    }
}

/* Keeps one socket open, sending what the program queues and handing on what arrives. */
fn serve_socket(url: &str, id: i64, token: u64, flow: &Shared, queued: Receiver<Outgoing>, io: &Sender<Msg>) {
    // No redirect is followed, a browser fails the socket on one and net.ts checked only this url.
    let mut socket = match tungstenite::client::connect_with_config(url, None, 0) {
        Ok((socket, _)) => socket,
        Err(e) => return answer(io, token, Err(e.to_string())),
    };
    let tcp: Option<&TcpStream> = match socket.get_ref() {
        MaybeTlsStream::Plain(tcp) => Some(tcp),
        MaybeTlsStream::Rustls(tls) => Some(tls.get_ref()),
        _ => None,
    };
    if let Some(tcp) = tcp {
        let _ = tcp.set_read_timeout(Some(SOCKET_POLL));
    }
    answer(io, token, Ok(json!(id)));
    loop {
        loop {
            let sent = match queued.try_recv() {
                Ok(Outgoing::Text(t)) => socket.send(Message::text(t)),
                Ok(Outgoing::Binary(b)) => socket.send(Message::binary(b)),
                Ok(Outgoing::Close) | Err(TryRecvError::Disconnected) => {
                    let _ = socket.close(None);
                    let _ = socket.flush();
                    return end(flow, io);
                }
                Err(TryRecvError::Empty) => break,
            };
            if sent.is_err() {
                return end(flow, io);
            }
        }
        match socket.read() {
            Ok(Message::Text(t)) => push(flow, io, json!(t.as_str())),
            Ok(Message::Binary(b)) => push(flow, io, to_json(&WireValue::Raw(b.to_vec()))),
            Ok(Message::Close(_)) => return end(flow, io),
            Ok(_) => {}
            Err(tungstenite::Error::Io(e)) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => return end(flow, io),
        }
    }
}
