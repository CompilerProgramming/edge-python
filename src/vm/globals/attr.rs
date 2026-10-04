use crate::s;

use alloc::string::{String, ToString};

use super::super::VM;
use super::super::types::*;

impl<'a> VM<'a> {

    // `getattr(obj, name [, default])` reads like `obj.name`, a default answers only an AttributeError.
    pub fn call_getattr(&mut self, op: u16, chunk: &crate::parser::SSAChunk, slots: &mut [Val]) -> Result<(), VmErr> {
        if op != 2 && op != 3 {
            return Err(cold_type("getattr() takes 2 or 3 arguments"));
        }
        let default = if op == 3 { Some(self.pop()?) } else { None };
        let name = self.expect_str_arg("getattr() name must be a string")?;
        let obj = self.pop()?;
        match (self.with_roots(default, |vm| vm.load_attr(obj, &name, chunk, slots)), default) {
            (Err(e), Some(d)) if self.absorb_attr_err(&e) => { self.push(d); Ok(()) }
            (r, _) => r,
        }
    }

    // `hasattr(obj, name)` is True when `getattr` would not raise AttributeError.
    pub fn call_hasattr(&mut self, chunk: &crate::parser::SSAChunk, slots: &mut [Val]) -> Result<(), VmErr> {
        let name = self.expect_str_arg("hasattr() name must be a string")?;
        let obj = self.pop()?;
        let found = match self.load_attr(obj, &name, chunk, slots) {
            Ok(()) => { self.pop()?; true }
            Err(e) if self.absorb_attr_err(&e) => false,
            Err(e) => return Err(e),
        };
        self.push(Val::bool(found));
        Ok(())
    }

    // `setattr(obj, name, value)` writes like `obj.name = value`, property setters included.
    pub fn call_setattr(&mut self, chunk: &crate::parser::SSAChunk, slots: &mut [Val]) -> Result<(), VmErr> {
        let value = self.pop()?;
        let name = self.expect_str_arg("setattr() name must be a string")?;
        let obj = self.pop()?;
        self.store_attr(obj, &name, value, chunk, slots)?;
        self.push(Val::none());
        Ok(())
    }

    /* `delattr(obj, name)`, remove an attribute from a user instance or class. */
    pub fn call_delattr(&mut self) -> Result<(), VmErr> {
        let name = self.expect_str_arg("delattr() name must be a string")?;
        let obj = self.pop()?;
        self.delete_attr_named(obj, &name)?;
        self.push(Val::none());
        Ok(())
    }

    /* `del obj.attr` opcode, pop the object, remove the named attribute in place. */
    pub fn exec_del_attr(&mut self, op: u16, chunk: &crate::parser::SSAChunk) -> Result<(), VmErr> {
        let obj = self.pop()?;
        let name = chunk.names.get(op as usize).ok_or_else(|| cold_runtime("DelAttr: bad name index"))?.clone();
        self.delete_attr_named(obj, &name)
    }

    /* Shared attribute removal for `delattr()` and `del obj.attr`, AttributeError when absent. */
    fn delete_attr_named(&mut self, obj: Val, name: &str) -> Result<(), VmErr> {
        let removed = match self.heap.try_get(obj) {
            Some(HeapObj::Class(_, _, members) | HeapObj::Func(_, _, _, members)) => {
                self.heap.growing(&mut *members.borrow_mut(), |m| {
                    let before = m.len();
                    m.retain(|(n, _)| n != name);
                    m.len() < before
                })
            }
            Some(HeapObj::Instance(_, attrs)) => {
                let key = attrs.borrow().iter().find(|(k, _)| matches!(self.heap.try_get(*k), Some(HeapObj::Str(s)) if s == name)).map(|(k, _)| k);
                key.is_some_and(|k| attrs.borrow_mut().remove(&k, &self.heap).is_some())
            }
            _ => return Err(cold_type("delattr() target must be an instance or class")),
        };
        // A cached result may have read a function attribute.
        if matches!(self.heap.get(obj), HeapObj::Func(..)) { self.templates.clear(); }
        if removed { return Ok(()); }
        Err(VmErr::Attribute(match self.heap.get(obj) {
            HeapObj::Class(n, ..) => s!("type object '", str n, "' has no attribute '", str name, "'"),
            _ => s!("'", str self.type_name(obj), "' object has no attribute '", str name, "'"),
        }))
    }

    // Returns v's String, or errors with `msg` when it isn't a heap string.
    pub(crate) fn str_of(&self, v: Val, msg: &'static str) -> Result<String, VmErr> {
        if v.is_heap() && let HeapObj::Str(s) = self.heap.get(v) { return Ok(s.clone()); }
        Err(cold_type(msg))
    }

    // Pops TOS and returns its String, or errors with `msg` if it isn't a heap string.
    fn expect_str_arg(&mut self, msg: &'static str) -> Result<String, VmErr> {
        let v = self.pop()?;
        self.str_of(v, msg)
    }

    /* `vars(obj)`, an Instance yields a copy of `__dict__`, a Module yields a dict from its attrs. No-arg form is unsupported, use `locals()`. */
    pub fn call_vars(&mut self) -> Result<(), VmErr> {
        use alloc::vec::Vec;
        let obj = self.pop()?;
        if !obj.is_heap() {
            return Err(cold_type("vars() requires an instance or module"));
        }
        // Two passes, drop the heap borrow before `alloc()`. Modules materialise names as `Vec<String>` first.
        enum Source { Instance(Vec<(Val, Val)>), Module(Vec<(String, Val)>) }
        let src = match self.heap.get(obj) {
            HeapObj::Instance(_, attrs) => Source::Instance(attrs.borrow().iter().collect()),
            HeapObj::Module(_, attrs) => Source::Module(attrs.clone()),
            _ => return Err(cold_type("vars() requires an instance or module")),
        };
        let entries: Vec<(Val, Val)> = match src {
            Source::Instance(e) => e,
            Source::Module(items) => {
                let mut out = Vec::with_capacity(items.len());
                for (name, v) in items {
                    let key = self.heap.alloc(HeapObj::Str(name))?;
                    out.push((key, v));
                }
                out
            }
        };
        let mut dm = DictMap::with_capacity(entries.len());
        for (k, v) in entries { dm.insert(k, v, &self.heap); }
        self.alloc_and_push_dict(dm)
    }

    /* `globals()`, module-level bindings as a dict. User top-level names only (entry-chunk slots + module state), builtins live in a separate namespace, matching Python. Returned dict is a copy. */
    pub fn call_globals(&mut self, chunk: &crate::parser::SSAChunk, slots: &[Val]) -> Result<(), VmErr> {
        let mut out: crate::util::hash::FxHashMap<String, Val> = crate::util::hash::FxHashMap::default();
        // Inside a function, entry slots sit at the bottom of `live_slots`, at top-level, use `slots` as-is.
        let (entry_chunk, entry_slots): (&crate::parser::SSAChunk, &[Val]) =
            if core::ptr::eq(chunk as *const _, self.chunk as *const _) {
                (chunk, slots)
            } else {
                let n = self.chunk.names.len().min(self.live_slots.len());
                (self.chunk, &self.live_slots[..n])
            };
        for (i, name) in entry_chunk.names.iter().enumerate() {
            if name.starts_with('#') { continue; }
            let v = match entry_slots.get(i) {
                Some(v) if !v.is_undef() => *v,
                _ => continue,
            };
            let bare = crate::parser::ssa_strip(name).to_string();
            // User assignment overrides the builtin entry of the same name.
            out.insert(bare, v);
        }
        // Module state (user-mutated via `global` from inside functions) overrides entry-chunk snapshots.
        for (k, v) in self.module_state.iter() {
            out.insert(k.clone(), *v);
        }
        let mut dm = DictMap::with_capacity(out.len());
        for (k, v) in out {
            let key = self.heap.alloc(HeapObj::Str(k))?;
            dm.insert(key, v, &self.heap);
        }
        self.alloc_and_push_dict(dm)
    }

    /* `locals()`, frame bindings as a dict. Dedupes SSA versions (`x_0`, `x_1`, ...) to the highest live one. Filters synthetic `#`-slots and unrebound builtins (same Val as the global). */
    pub fn call_locals(&mut self, chunk: &crate::parser::SSAChunk, slots: &[Val]) -> Result<(), VmErr> {
        // Map bare-name -> (best version, val) so we keep only the latest.
        let mut latest: crate::util::hash::FxHashMap<String, (i64, Val)> = crate::util::hash::FxHashMap::default();
        for (i, name) in chunk.names.iter().enumerate() {
            let v = match slots.get(i) {
                Some(v) if !v.is_undef() => *v,
                _ => continue,
            };
            // Synthetic `#`-slots are matcher scratch, never user-visible.
            if name.starts_with('#') { continue; }
            // Strip SSA version suffix.
            let (bare, ver) = crate::parser::SsaName::parse_or_bare(name);
            let ver = ver as i64;
            // Skip unrebound builtins, same Val as the global means the user never assigned locally.
            if let Some(gv) = self.global(bare)
                && gv.0 == v.0 { continue; }
            let entry = latest.entry(bare.to_string()).or_insert((-1, Val::undef()));
            if ver > entry.0 { *entry = (ver, v); }
        }
        let mut dm = DictMap::with_capacity(latest.len());
        for (name, (_, v)) in latest {
            let key = self.heap.alloc(HeapObj::Str(name))?;
            dm.insert(key, v, &self.heap);
        }
        self.alloc_and_push_dict(dm)
    }
}
