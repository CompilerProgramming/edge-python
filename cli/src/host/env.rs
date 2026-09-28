use super::js::Called;
use super::{read, read_u32, rt, stage, unstage, write, write_u32, Exports, Native, Plugin, State};
use anyhow::{anyhow, Result};
use compiler::abi::WireValue;
use wasmtime::{AsContextMut, Caller, Linker};

// The RUNTIME error kind of the ABI.
pub const ERR_RUNTIME: i32 = 2;
pub const ERR_CUSTOM: i32 = 6;

/* The five `env` imports compiler.wasm declares, bound to the store state. */
pub fn link(linker: &mut Linker<State>) -> Result<()> {
    linker
        .func_wrap("env", "host_print", |mut caller: Caller<'_, State>, ptr: i32, len: i32| {
            let ex = exports(&caller);
            let text = String::from_utf8_lossy(&read(&mut caller, ex.memory, ptr, len)).into_owned();
            if let Ok(mut print) = caller.data().print.lock() {
                print(&text);
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    linker
        .func_wrap("env", "host_now_ns", |_: Caller<'_, State>| -> i64 { super::now_ns() as i64 })
        .map_err(|e| anyhow!("{e}"))?;
    linker
        .func_wrap("env", "host_fetch_bytes", |mut caller: Caller<'_, State>, spec_ptr: i32, spec_len: i32, _hash_ptr: i32, out_len: i32| -> wasmtime::Result<i32> {
            let ex = exports(&caller);
            let spec = String::from_utf8_lossy(&read(&mut caller, ex.memory, spec_ptr, spec_len)).into_owned();
            let Some(bytes) = caller.data().fetched.get(&spec).cloned() else {
                write_u32(&mut caller, ex.memory, out_len, 0);
                return Ok(0);
            };
            // The compiler frees this with wasm_free and the same length.
            let ptr = ex.wasm_alloc.call(&mut caller, bytes.len() as i32)?;
            write(&mut caller, ex.memory, ptr, &bytes);
            write_u32(&mut caller, ex.memory, out_len, bytes.len() as u32);
            Ok(ptr)
        })
        .map_err(|e| anyhow!("{e}"))?;
    linker
        .func_wrap("env", "host_send", |mut caller: Caller<'_, State>, group_ptr: i32, group_len: i32, body_ptr: i32, body_len: i32| -> i32 {
            let ex = exports(&caller);
            let group = String::from_utf8_lossy(&read(&mut caller, ex.memory, group_ptr, group_len)).into_owned();
            let body = String::from_utf8_lossy(&read(&mut caller, ex.memory, body_ptr, body_len)).into_owned();
            match caller.data_mut().outbox.as_mut() {
                Some(outbox) => {
                    outbox.push((group, body));
                    0
                }
                None => 1,
            }
        })
        .map_err(|e| anyhow!("{e}"))?;
    linker
        .func_wrap("env", "host_call_native", |mut caller: Caller<'_, State>, id: i32, call_id: i32, argv_ptr: i32, argc: i32, out_ptr: i32| -> wasmtime::Result<i32> {
            call_native(&mut caller, id, call_id, argv_ptr, argc, out_ptr)
        })
        .map_err(|e| anyhow!("{e}"))?;
    Ok(())
}

fn exports(caller: &Caller<'_, State>) -> Exports {
    caller.data().exports.clone().expect("compiler exports bound before any host call")
}

/* Dispatches one extern call, plugins get staged argv, JavaScript exports get decoded values. */
fn call_native(caller: &mut Caller<'_, State>, id: i32, call_id: i32, argv_ptr: i32, argc: i32, out_ptr: i32) -> wasmtime::Result<i32> {
    let ex = exports(caller);
    let Some(native) = caller.data().natives.get(id as usize).cloned() else {
        throw(caller, &ex, &format!("native id {id} not registered"));
        return Ok(1);
    };
    match native {
        Native::Plugin(plugin) => {
            let argv = read(caller, ex.memory, argv_ptr, argc * 4);
            let len = (argc * 4).max(4);
            let g_argv = plugin.alloc.call(&mut *caller, len)?;
            let g_out = plugin.alloc.call(&mut *caller, 4)?;
            write(caller, plugin.memory, g_argv, &argv);
            caller.data_mut().running.push(call_id as u32);
            let called = plugin.func.call(&mut *caller, (g_argv, argc, g_out));
            caller.data_mut().running.pop();
            let mut status = match called {
                Ok(status) => status,
                Err(e) => {
                    throw(caller, &ex, &format!("native module trapped: {e}"));
                    return Ok(1);
                }
            };
            if status == 0 {
                let handle = read_u32(caller, plugin.memory, g_out);
                write_u32(caller, ex.memory, out_ptr, handle);
            }
            if let Some(free) = &plugin.free {
                let _ = free.call(&mut *caller, (g_argv, len));
                let _ = free.call(&mut *caller, (g_out, 4));
            }
            // A plugin waiting on a system call finishes in its resume export once that settles.
            if status == 2 && plugin.resume.is_none() {
                throw(caller, &ex, "native module waits on a system call but exports no __edge_resume");
                status = 1;
            }
            if status == 2 {
                caller.data_mut().waiting.insert(call_id as u32, plugin);
            }
            Ok(status)
        }
        Native::System { module, name, package } => {
            let raw = read(caller, ex.memory, argv_ptr, argc.max(1) * 4);
            let handles: Vec<u32> = raw.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect();
            // The trailing slot holds the kwargs, which a system call never takes.
            let Some((&0, positional)) = handles.split_last() else {
                throw(caller, &ex, &format!("TypeError: {module}.{name} takes positional arguments only"));
                return Ok(1);
            };
            let mut args = Vec::with_capacity(positional.len());
            for &handle in positional {
                match rt::decode(caller, &ex, handle) {
                    Ok(value) => args.push(value),
                    Err(e) => {
                        throw(caller, &ex, &e);
                        return Ok(1);
                    }
                }
            }
            let state = caller.data();
            let called = match state.events.clone() {
                Some(events) => super::system::invoke(state.run, &package, &module, &name, &args, call_id as u32, events),
                None => Err(format!("RuntimeError: {module}.{name} has no interpreter to answer")),
            };
            let answer = called.map(|called| match called {
                super::system::Called::Value(value) => Some(value),
                super::system::Called::Pending => None,
            });
            answered(caller, &ex, out_ptr, call_id, answer)
        }
        Native::Js { runtime, name } => {
            // The trailing kwargs slot is dropped, JavaScript exports take positional values.
            let raw = read(caller, ex.memory, argv_ptr, (argc - 1).max(0) * 4);
            let mut args = Vec::with_capacity(raw.len() / 4);
            for handle in raw.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)) {
                match rt::decode(caller, &ex, handle) {
                    Ok(value) => args.push(value),
                    Err(e) => {
                        throw(caller, &ex, &e);
                        return Ok(1);
                    }
                }
            }
            let state = caller.data_mut();
            let (slot, deadline) = (state.slot, state.deadline);
            let called = match (state.events.clone(), state.js.get_mut(runtime)) {
                (Some(events), Some(js)) => js.call(slot, &name, args, events, call_id as u32, deadline),
                _ => Err(format!("{name} has no interpreter to answer")),
            };
            match called.and_then(|called| match called {
                Called::Value(value) => rt::encode(caller, &ex, &value).map(Some),
                Called::Pending => Ok(None),
            }) {
                Ok(Some(handle)) => {
                    write_u32(caller, ex.memory, out_ptr, handle);
                    Ok(0)
                }
                Ok(None) => {
                    caller.data_mut().deferred.push(call_id as u32);
                    Ok(2)
                }
                Err(e) => {
                    throw(caller, &ex, &e);
                    Ok(1)
                }
            }
        }
    }
}

/* Hands a call's answer to the compiler, a value now, a park for one that settles later, or an error. */
fn answered(caller: &mut Caller<'_, State>, ex: &Exports, out_ptr: i32, call_id: i32, answer: Result<Option<WireValue>, String>) -> wasmtime::Result<i32> {
    match answer.and_then(|value| value.map(|v| rt::encode(caller, ex, &v)).transpose()) {
        Ok(Some(handle)) => {
            write_u32(caller, ex.memory, out_ptr, handle);
            Ok(0)
        }
        Ok(None) => {
            caller.data_mut().deferred.push(call_id as u32);
            Ok(2)
        }
        Err(e) => {
            throw(caller, ex, e.as_str());
            Ok(1)
        }
    }
}

/* Hands a settled system call to its waiting plugin, then delivers what the plugin answers. */
pub(super) fn resume(cx: &mut impl AsContextMut<Data = State>, ex: &Exports, id: u32, plugin: Box<Plugin>, settled: Result<WireValue, String>) -> wasmtime::Result<i32> {
    let Some(resume) = plugin.resume.clone() else { return Ok(2) };
    // A failed call reaches the plugin as a zero handle with its error stashed.
    let answer = match settled {
        Ok(value) => rt::encode(cx, ex, &value).map_err(|e| wasmtime::format_err!("{e}"))? as i32,
        Err(msg) => {
            throw(cx, ex, &msg);
            0
        }
    };
    let out = plugin.alloc.call(&mut *cx, 4)?;
    cx.as_context_mut().data_mut().running.push(id);
    let called = resume.call(&mut *cx, (id as i32, answer, out));
    cx.as_context_mut().data_mut().running.pop();
    let status = called.unwrap_or_else(|e| {
        throw(cx, ex, &format!("native module trapped: {e}"));
        1
    });
    let handle = read_u32(cx, plugin.memory, out);
    if let Some(free) = &plugin.free {
        let _ = free.call(&mut *cx, (out, 4));
    }
    match status {
        0 => ex.set_host_result_by_id.call(&mut *cx, (id as i32, handle as i32)),
        // Still waiting, the system call it just made settles on the same id.
        2 => {
            cx.as_context_mut().data_mut().waiting.insert(id, plugin);
            Ok(0)
        }
        _ => {
            let (kind, msg) = take_error(cx, ex)?;
            let handle = rt::encode(cx, ex, &WireValue::Bytes(msg.into_bytes())).map_err(|e| wasmtime::format_err!("{e}"))?;
            ex.set_host_error_by_id.call(&mut *cx, (id as i32, kind, handle as i32))
        }
    }
}

/* The error a plugin stashed, as its kind and message. */
fn take_error(cx: &mut impl AsContextMut<Data = State>, ex: &Exports) -> wasmtime::Result<(i32, String)> {
    let mut size = 256;
    loop {
        let kind = ex.wasm_alloc.call(&mut *cx, 4)?;
        let buf = ex.wasm_alloc.call(&mut *cx, size)?;
        let got = ex.host_edge_take_error.call(&mut *cx, (kind, buf, size))?;
        let taken = (read_u32(cx, ex.memory, kind) as i32, String::from_utf8_lossy(&read(cx, ex.memory, buf, got.max(0))).into_owned());
        let _ = ex.wasm_free.call(&mut *cx, (kind, 4));
        let _ = ex.wasm_free.call(&mut *cx, (buf, size));
        match got {
            n if n >= 0 => return Ok(taken),
            -1 => return Ok((ERR_RUNTIME, "native call failed".to_string())),
            n => size = -n,
        }
    }
}

/* The error kind a message raises as, picked by its class prefix, a custom kind keeps the class in the message. */
pub(super) fn kind_of(msg: &str) -> (i32, &str) {
    match msg.split_once(": ") {
        Some(("TypeError", rest)) => (0, rest),
        Some(("ValueError", rest)) => (1, rest),
        Some(("RuntimeError", rest)) => (ERR_RUNTIME, rest),
        Some(("OSError" | "PermissionError" | "TimeoutError", _)) => (ERR_CUSTOM, msg),
        _ => (ERR_RUNTIME, msg),
    }
}

/* Stashes the error the compiler raises once the call returns 1, a class prefix picks its kind. */
pub(super) fn throw(caller: &mut impl AsContextMut<Data = State>, ex: &Exports, msg: &str) {
    let (kind, msg) = kind_of(msg);
    if let Ok(ptr) = stage(caller, ex, msg.as_bytes()) {
        let _ = ex.host_edge_throw.call(&mut *caller, (kind, ptr, msg.len() as i32));
        unstage(caller, ex, ptr, msg.len());
    }
}
