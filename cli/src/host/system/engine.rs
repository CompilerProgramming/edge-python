use super::pipe::{epoch, Pipe};
use super::{fault_text, from_json, Msg};
use crate::host::Completion;
use compiler::modules::{dir_of, join_relative};
use mozjs::context::JSContext;
use mozjs::conversions::jsstr_to_string;
use mozjs::jsapi::{self, CallArgs, Handle, HandleValueArray, JSObject, JSScript, MutableHandle, OnNewGlobalHookOption, Value};
use mozjs::jsval::{StringValue, UndefinedValue};
use mozjs::rooted;
use mozjs::rust::wrappers2::*;
use mozjs::rust::{evaluate_script, transform_str_to_source_text, CompileOptionsWrapper, JSEngine, RealmOptions, Runtime, SIMPLE_GLOBAL_CLASS};
use serde_json::Value as Json;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::ptr::{self, NonNull};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Mutex;
use std::thread::JoinHandle;

const WEB: &str = include_str!("web.js");
// The only JavaScript that can ever load, the system calls `js/dist` carries.
const SYSTEM_DIR: &str = "src/system/";
const ENTRY: &str = "import { SYSTEM } from './system/index.js';\nimport { check, scopes, unmet } from './system/grants.js';\nglobalThis.__edge = { open: SYSTEM, check, scopes, unmet };\n";

/* What the thread keeps between calls, the calls waiting to settle and the pipe's requests and sockets. */
struct State {
    io: Sender<Msg>,
    settles: HashMap<u64, (u32, Sender<Completion>)>,
    pipe: Pipe,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

// The thread the exit hook waits on, since the static holding its sender never drops.
static HANDLE: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

unsafe extern "C" {
    fn atexit(hook: extern "C" fn()) -> i32;
}

/* Starts the one thread SpiderMonkey runs on, every system call reaches it through the sender. */
pub(super) fn spawn() -> Sender<Msg> {
    let (tx, rx) = channel();
    let io = tx.clone();
    let thread = std::thread::Builder::new().name("edge-system".into()).spawn(move || serve(rx, io)).expect("spawning the system call thread");
    if let Ok(mut handle) = HANDLE.lock() {
        *handle = Some(thread);
    }
    tx
}

/* Exit destroys SpiderMonkey's statics, which it cannot survive running, so the thread shuts it down first. */
extern "C" fn stop() {
    let Some(thread) = HANDLE.lock().ok().and_then(|mut handle| handle.take()) else { return };
    if let Some(tx) = super::THREAD.get() {
        let _ = tx.send(Msg::Stop);
    }
    let _ = thread.join();
}

fn serve(inbox: Receiver<Msg>, io: Sender<Msg>) {
    // The monotonic clock starts with the thread, so a first reading is never the tick it started on.
    epoch();
    STATE.set(Some(State { io, settles: HashMap::new(), pipe: Pipe::default() }));
    let engine = JSEngine::init().expect("starting SpiderMonkey");
    // Registered after init, so the hook runs before exit destroys what init created.
    unsafe { atexit(stop) };
    let mut rt = Runtime::new(engine.handle());
    let runtime = rt.rt();
    let cx = rt.cx();
    unsafe {
        jsapi::JS::SetJobQueue(cx.raw_cx(), mozjs::glue::CreateJobQueue(&TRAPS) as *mut jsapi::JS::JobQueue);
        jsapi::JS::SetModuleLoadHook(runtime, Some(load));
    }
    let options = RealmOptions::default();
    rooted!(&in(cx) let global = unsafe { JS_NewGlobalObject(cx, &SIMPLE_GLOBAL_CLASS, ptr::null_mut(), OnNewGlobalHookOption::FireOnNewGlobalHook, &*options) });
    let _realm = jsapi::JSAutoRealm::new(unsafe { cx.raw_cx() }, global.get());
    unsafe { JS_DefineFunction(cx, global.handle(), c"__pipe".as_ptr(), Some(pipe), 2, 0) };
    rooted!(&in(cx) let mut ignored = UndefinedValue());
    let compiled = CompileOptionsWrapper::new(cx, c"web.js".to_owned(), 1);
    evaluate_script(cx, global.handle(), WEB, ignored.handle_mut(), compiled).expect("web.js is the host's own script");
    unsafe { evaluate_module(cx, "src/entry.js", ENTRY) };
    for message in inbox {
        let (function, json, reply) = match message {
            Msg::Bridge { function, json, reply, settle } => {
                if let Some((key, id, events)) = settle {
                    with_state(|s| s.settles.insert(key, (id, events)));
                }
                (function, json, Some(reply))
            }
            Msg::Io(json) => (c"__edge_io", json, None),
            // Leaving drops the runtime and then the engine, which shuts SpiderMonkey down.
            Msg::Stop => break,
        };
        let answer = unsafe { call(cx, global.handle(), function, &json) };
        unsafe { drain(cx.raw_cx()) };
        if let Some(reply) = reply {
            let _ = reply.send(answer);
        }
    }
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> R {
    STATE.with(|s| f(s.borrow_mut().as_mut().expect("the system call thread keeps its state")))
}

unsafe fn text(cx: &mut JSContext, v: Handle<Value>) -> String {
    let s = unsafe { mozjs::rust::ToString(cx, mozjs::rust::Handle::from_raw(v)) };
    NonNull::new(s).map_or_else(String::new, |s| unsafe { jsstr_to_string(cx, s) })
}

unsafe fn string(cx: &mut JSContext, s: &str) -> *mut jsapi::JSString {
    // Read as UTF-8, since the plain copy takes every byte for a Latin-1 character.
    let chars = mozjs::conversions::Utf8Chars::from(s);
    unsafe { JS_NewStringCopyUTF8N(cx, &*chars as *const _) }
}

/* Calls a bridge function of `web.js` with one JSON string, its JSON answer back. */
unsafe fn call(cx: &mut JSContext, global: mozjs::rust::HandleObject, function: &CStr, json: &str) -> String {
    rooted!(&in(cx) let arg = StringValue(unsafe { &*string(cx, json) }));
    rooted!(&in(cx) let mut out = UndefinedValue());
    let args = HandleValueArray { length_: 1, elements_: &*arg };
    if !unsafe { JS_CallFunctionName(cx, global, function.as_ptr(), &args, out.handle_mut()) } {
        return serde_json::json!({ "error": ["RuntimeError", "the system call bridge threw"] }).to_string();
    }
    unsafe { text(cx, out.handle().into()) }
}

/* Compiles one module of the host's own and names it by its key, so its imports resolve beside it. */
unsafe fn compile(cx: &mut JSContext, key: &str, src: &str) -> *mut JSObject {
    let options = CompileOptionsWrapper::new(cx, CString::new(key).unwrap_or_default(), 1);
    let mut source = transform_str_to_source_text(src);
    let module = unsafe { CompileModule1(cx, options.ptr, &mut source) };
    if !module.is_null() {
        let name = unsafe { string(cx, key) };
        unsafe { jsapi::JS::SetModulePrivate(module, &StringValue(&*name)) };
    }
    module
}

/* Loads the entry and every module it imports, then runs it, which installs the bridge `web.js` calls. */
unsafe fn evaluate_module(cx: &mut JSContext, key: &str, src: &str) {
    rooted!(&in(cx) let entry = unsafe { compile(cx, key, src) });
    rooted!(&in(cx) let mut loaded = ptr::null_mut::<JSObject>());
    let undefined = unsafe { mozjs::rust::Handle::from_raw(jsapi::JS::UndefinedHandleValue) };
    assert!(unsafe { LoadRequestedModules1(cx, entry.handle(), undefined, loaded.handle_mut()) }, "loading the system modules");
    unsafe { drain(cx.raw_cx()) };
    assert!(unsafe { ModuleLink(cx, entry.handle()) }, "linking the system modules");
    rooted!(&in(cx) let mut done = UndefinedValue());
    assert!(unsafe { ModuleEvaluate(cx, entry.handle(), done.handle_mut()) }, "running the system modules");
    unsafe { drain(cx.raw_cx()) };
}

/* Answers an import with a module of `js/dist`, joined the way Edge Python joins its own, and refuses anything outside the system calls. */
unsafe extern "C" fn load(cx: *mut jsapi::JSContext, referrer: Handle<*mut JSScript>, request: Handle<*mut JSObject>, _host: Handle<Value>, payload: Handle<Value>, _line: u32, _column: jsapi::JS::ColumnNumberOneOrigin) -> bool {
    let Some(raw) = NonNull::new(cx) else { return false };
    let mut cx = unsafe { JSContext::from_ptr(raw) };
    let (referrer, request, payload) = unsafe { (mozjs::rust::Handle::from_raw(referrer), mozjs::rust::Handle::from_raw(request), mozjs::rust::Handle::from_raw(payload)) };
    rooted!(&in(cx) let module = unsafe { GetModuleObject(referrer) });
    rooted!(&in(cx) let mut private = UndefinedValue());
    unsafe { mozjs::glue::JS_GetModulePrivate(module.get(), private.handle_mut().into()) };
    let from = unsafe { text(&mut cx, private.handle().into()) };
    let specifier = NonNull::new(unsafe { GetModuleRequestSpecifier(&cx, request) }).map_or_else(String::new, |s| unsafe { jsstr_to_string(&cx, s) });
    let key = join_relative(&dir_of(&from), &specifier);
    let source = key.starts_with(SYSTEM_DIR).then(|| crate::web::JS_HOST.iter().find(|(k, _)| *k == key)).flatten().map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned());
    let Some(source) = source else { return false };
    rooted!(&in(cx) let target = unsafe { compile(&mut cx, &key, &source) });
    !target.get().is_null() && unsafe { FinishLoadingImportedModule(&mut cx, referrer, request, payload, target.handle(), false) }
}

/* The one door out of JavaScript, `web.js` asks the host for a pipe operation or reports a settled call. */
unsafe extern "C" fn pipe(cx: *mut jsapi::JSContext, argc: u32, vp: *mut Value) -> bool {
    let Some(raw) = NonNull::new(cx) else { return false };
    let args = unsafe { CallArgs::from_vp(vp, argc) };
    let mut cx = unsafe { JSContext::from_ptr(raw) };
    let op = unsafe { text(&mut cx, args.get(0)) };
    let json: Json = serde_json::from_str(&unsafe { text(&mut cx, args.get(1)) }).unwrap_or(Json::Null);
    let answer = with_state(|state| {
        if op == "settle" {
            settle(state, &json);
            return serde_json::json!({});
        }
        match state.pipe.handle(&op, &json, &state.io) {
            Ok(value) => serde_json::json!({ "value": value }),
            Err(error) => serde_json::json!({ "error": error }),
        }
    });
    let out = unsafe { string(&mut cx, &answer.to_string()) };
    args.rval().set(StringValue(unsafe { &*out }));
    true
}

/* A call that answered with a promise settled, its completion goes to the interpreter that parked on it. */
fn settle(state: &mut State, json: &Json) {
    let Some((id, events)) = json["call"].as_u64().and_then(|key| state.settles.remove(&key)) else { return };
    let completion = match json.get("error") {
        Some(error) => Completion::Error { id, msg: fault_text(error) },
        None => Completion::Value { id, value: from_json(&json["value"]) },
    };
    let _ = events.send(completion);
}

/* Runs every queued microtask, the reactions a call or a finished pipe operation left. */
unsafe fn drain(cx: *mut jsapi::JSContext) {
    unsafe {
        while jsapi::JS::HasRegularMicroTasks(cx) {
            let task = jsapi::JS::DequeueNextRegularMicroTask(cx);
            if !jsapi::JS::IsJSMicroTask(&task) {
                continue;
            }
            let job = jsapi::JS::ToUnwrappedJSMicroTask(&task);
            let _ = jsapi::JS::RunJSMicroTask(cx, Handle::from_marked_location(&job));
        }
    }
}

unsafe extern "C" fn host_data(_cx: *mut jsapi::JSContext, _incumbent: MutableHandle<*mut JSObject>, _data: MutableHandle<*mut JSObject>) -> bool {
    true
}

unsafe extern "C" fn host_global(_cx: *mut jsapi::JSContext, _data: MutableHandle<*mut JSObject>) -> bool {
    true
}

unsafe extern "C" fn run_jobs(cx: *mut jsapi::JSContext) {
    unsafe { drain(cx) }
}

unsafe extern "C" fn trace_task(_trc: *mut jsapi::JSTracer, _value: *mut Value) {}

// The host's own microtask queue, SpiderMonkey's internal one has to be chosen before mozjs starts it.
static TRAPS: mozjs::glue::JobQueueTraps = mozjs::glue::JobQueueTraps {
    getHostDefinedData: Some(host_data),
    getHostDefinedGlobal: Some(host_global),
    runJobs: Some(run_jobs),
    traceNonGCThingMicroTask: Some(trace_task),
};
