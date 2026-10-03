use super::types::{Val, HeapObj, HeapPool, VmErr, EQ_DEPTH_MAX};
use crate::parser::{OpCode, SSAChunk, Instruction, Value};

use alloc::{vec, vec::Vec, string::ToString};

/* Type-specialised binop variants reachable from the inline cache. */
#[derive(Debug, Clone, Copy)]
pub enum FastOp {
    AddInt, AddFloat, AddStr,
    SubInt, SubFloat,
    MulInt, MulFloat,
    LtInt, LtFloat,
    GtInt, LtEqInt, GtEqInt,
    EqInt, EqStr,
    NotEqInt,
    ModInt, FloorDivInt
}

/* Promote to `fast` after this many hits with a stable type key. */
const QUICK_THRESH: u8 = 4;

/* Per-site monomorphic instance-dunder cache. Records the receiver's class heap idx and the pre-resolved method Val, once `hits >= QUICK_THRESH` the slot promotes and the hot dispatch skips the class lookup entirely. `arity` is the total operand count consumed from the stack (1 for unary, 2 for binary like `__add__`/`__getitem__`). */
#[derive(Clone, Copy)]
pub struct InstanceCache {
    pub class: u32,
    // The class that defines the method, what `super()` inside it resumes from.
    pub owner: u32,
    pub method_bits: u64,
    pub arity: u8,
    hits: u8,
    promoted: bool,
}

#[derive(Clone, Default)]
struct CacheSlot {
    type_key: u8,
    hits: u8,
    fast: Option<FastOp>,
    // Instance-dunder cache, orthogonal to `fast`, dispatch checks it after scalar specialisation misses.
    inst: Option<InstanceCache>,
}

pub struct OpcodeCache {
    slots: Vec<CacheSlot>,
    fused: Option<Vec<Instruction>>,
    /* Pre-materialised const pool so LoadConst is one indexed load, no per-iter alloc. */
    const_vals: Option<Vec<Val>>,
    /* Length of each name without its version suffix, cut on the first module-scope access. */
    bare_len: Option<Vec<u32>>,
}

impl OpcodeCache {
    pub fn new(chunk: &SSAChunk) -> Self {
        Self {
            slots: vec![CacheSlot::default(); chunk.instructions.len()],
            fused: None,
            const_vals: None,
            bare_len: None,
        }
    }

    /* The bare name of `names[i]`, its version suffix cut once per chunk rather than on every access. */
    pub fn bare<'c>(&mut self, chunk: &'c SSAChunk, i: usize) -> Option<&'c str> {
        let lens = self.bare_len.get_or_insert_with(|| chunk.names.iter().map(|n| crate::parser::ssa_strip(n).len() as u32).collect());
        let len = *lens.get(i)? as usize;
        chunk.names.get(i).map(|n| &n[..len])
    }

    /* Compile the fused instruction stream on first access, reuse afterwards. */
    pub fn ensure_fused(&mut self, chunk: &SSAChunk) -> &[Instruction] {
        if self.fused.is_none() {
            self.fused = Some(fuse_method_calls(chunk));
        }
        self.fused.as_ref().unwrap()
    }

    /* Direct access (caller must have called ensure_fused). */
    pub fn fused_ref(&self) -> &[Instruction] {
        self.fused.as_ref().expect("fused code not compiled")
    }

    /* Build the const pool, scalars inline, Str/LongInt heap-allocated once and shared. */
    pub fn ensure_const_vals(&mut self, chunk: &SSAChunk, heap: &mut HeapPool) -> Result<&[Val], VmErr> {
        if self.const_vals.is_none() {
            let mut out = Vec::with_capacity(chunk.constants.len());
            for c in &chunk.constants {
                let v = match c {
                    // A wide literal that fits inline demotes so hash and eq stay in sync with the short form.
                    Value::Int(i) => heap.int(*i as i128)?,
                    Value::LongInt(i) => heap.int(*i)?,
                    Value::Float(f) => Val::float(*f),
                    Value::Bool(b) => Val::bool(*b),
                    Value::None => Val::none(),
                    Value::Str(s) => heap.alloc(HeapObj::Str(s.to_string()))?,
                    Value::Bytes(b) => heap.alloc(HeapObj::Bytes(b.clone()))?,
                };
                out.push(v);
            }
            self.const_vals = Some(out);
        }
        Ok(self.const_vals.as_ref().unwrap())
    }

    /* Direct access (caller must have called ensure_const_vals). */
    pub fn const_vals_ref(&self) -> &[Val] {
        self.const_vals.as_ref().expect("const pool not materialized")
    }

    pub fn const_vals_opt(&self) -> Option<&[Val]> {
        self.const_vals.as_deref()
    }

    pub fn record(&mut self, ip: usize, opcode: &OpCode, ta: u8, tb: u8) {
        let Some(s) = self.slots.get_mut(ip) else { return };
        let key = (ta << 4) | (tb & 0xF);
        if s.type_key == key {
            s.hits = s.hits.saturating_add(1);
            if s.hits >= QUICK_THRESH && s.fast.is_none() {
                s.fast = Self::specialize(opcode, ta, tb);
            }
        } else {
            // Preserve `inst`, its lifecycle is independent of scalar specialisation.
            s.type_key = key;
            s.hits = 1;
            s.fast = None;
        }
    }

    #[inline]
    pub fn get_fast(&self, ip: usize) -> Option<FastOp> {
        self.slots.get(ip).and_then(|s| s.fast)
    }

    pub fn invalidate(&mut self, ip: usize) {
        // Preserve `inst` so the instance-dunder cache survives a scalar specialisation miss at the same site.
        if let Some(s) = self.slots.get_mut(ip) {
            s.type_key = 0;
            s.hits = 0;
            s.fast = None;
        }
    }

    /* Monomorphic instance-dunder hit counter, promotes after `QUICK_THRESH` consecutive hits with the same class + method pair. Polymorphic sites churn (`record_inst` overwrites on mismatch) but never wedge. */
    pub fn record_inst(&mut self, ip: usize, class: u32, owner: u32, method: Val, arity: u8) {
        let Some(s) = self.slots.get_mut(ip) else { return };
        match s.inst.as_mut() {
            Some(c) if c.class == class && c.method_bits == method.0 && c.arity == arity => {
                c.hits = c.hits.saturating_add(1);
                if c.hits >= QUICK_THRESH { c.promoted = true; }
            }
            _ => {
                s.inst = Some(InstanceCache {
                    class,
                    owner,
                    method_bits: method.0,
                    arity,
                    hits: 1,
                    promoted: false,
                });
            }
        }
    }

    #[inline]
    pub fn get_inst(&self, ip: usize) -> Option<InstanceCache> {
        self.slots.get(ip).and_then(|s| s.inst).filter(|c| c.promoted)
    }

    pub fn invalidate_inst(&mut self, ip: usize) {
        if let Some(s) = self.slots.get_mut(ip) { s.inst = None; }
    }

    /* GC root iterator for `InstanceCache` entries, yielding the cached method Val and class Val so the collector keeps both alive while the cache holds them. */
    pub fn inst_roots(&self) -> impl Iterator<Item = Val> + '_ {
        self.slots.iter().filter_map(|s| s.inst).flat_map(|c| {
            // SAFETY `method_bits` was recorded from a live `Val`, class Val is reconstructed from the stored heap idx.
            let method = unsafe { Val::from_raw(c.method_bits) };
            [method, Val::heap(c.class), Val::heap(c.owner)].into_iter()
        })
    }

    fn specialize(opcode: &OpCode, ta: u8, tb: u8) -> Option<FastOp> {
        match (opcode, ta, tb) {
            (OpCode::Add, 1, 1) => Some(FastOp::AddInt), (OpCode::Add, 2, 2) => Some(FastOp::AddFloat),
            (OpCode::Add, 3, 3) => Some(FastOp::AddStr), (OpCode::Sub, 1, 1) => Some(FastOp::SubInt),
            (OpCode::Sub, 2, 2) => Some(FastOp::SubFloat), (OpCode::Mul, 1, 1) => Some(FastOp::MulInt),
            (OpCode::Mul, 2, 2) => Some(FastOp::MulFloat), (OpCode::Lt, 1, 1) => Some(FastOp::LtInt),
            (OpCode::Lt, 2, 2) => Some(FastOp::LtFloat), (OpCode::Eq, 1, 1) => Some(FastOp::EqInt),
            (OpCode::Eq, 3, 3) => Some(FastOp::EqStr), (OpCode::Gt, 1, 1) => Some(FastOp::GtInt),
            (OpCode::LtEq, 1, 1) => Some(FastOp::LtEqInt), (OpCode::GtEq, 1, 1) => Some(FastOp::GtEqInt),
            (OpCode::NotEq, 1, 1) => Some(FastOp::NotEqInt),
            (OpCode::Mod, 1, 1) => Some(FastOp::ModInt),
            (OpCode::FloorDiv, 1, 1) => Some(FastOp::FloorDivInt),
            _ => None,
        }
    }
}

// Template memoization for pure functions.

fn args_match(e: &TplEntry, args: &[Val], owner: Val, h: u64, heap: &HeapPool) -> bool {
    e.hash == h
    && e.owner.0 == owner.0
    && e.args.len() == args.len()
    && e.args.iter().zip(args).all(|(&a, &b)| key_eq(a, b, heap, 0))
}

// `owner` is the function when it has defaults, so other defaults never share a result.
struct TplEntry { args: Vec<Val>, owner: Val, result: Val, hash: u64 }

fn mix(h: u64, x: u64) -> u64 { (h ^ x).wrapping_mul(0x100000001b3) }

fn hash_args(args: &[Val], heap: &HeapPool) -> u64 {
    args.iter().fold(0xcbf29ce484222325, |h, &v| mix(h, if v.is_heap() { key_hash(v, heap, 0) } else { v.0 }))
}

/* Long strings, bytes and tuples hash by content, everything else by its bits. */
#[inline]
fn key_hash(v: Val, heap: &HeapPool, depth: usize) -> u64 {
    let fold = |seed: u64, bytes: &[u8]| bytes.iter().fold(seed, |h, &b| mix(h, b as u64));
    if !v.is_heap() || depth > EQ_DEPTH_MAX { return v.0; }
    match heap.try_get(v) {
        Some(HeapObj::Str(s)) if s.len() > 128 => fold(1, s.as_bytes()),
        Some(HeapObj::Bytes(b)) if b.len() > 128 => fold(2, b),
        Some(HeapObj::Tuple(items)) => items.iter().fold(3, |h, &x| mix(h, key_hash(x, heap, depth + 1))),
        _ => v.0,
    }
}

/* Strict equality, so `1`, `1.0` and `True` stay apart. */
#[inline]
fn key_eq(a: Val, b: Val, heap: &HeapPool, depth: usize) -> bool {
    if a.0 == b.0 { return true; }
    if !a.is_heap() || !b.is_heap() || depth > EQ_DEPTH_MAX { return false; }
    match (heap.try_get(a), heap.try_get(b)) {
        (Some(HeapObj::Str(x)), Some(HeapObj::Str(y))) => x == y,
        (Some(HeapObj::Bytes(x)), Some(HeapObj::Bytes(y))) => x == y,
        (Some(HeapObj::Tuple(x)), Some(HeapObj::Tuple(y))) => x.len() == y.len() && x.iter().zip(y).all(|(&p, &q)| key_eq(p, q, heap, depth + 1)),
        _ => false,
    }
}

/* Immutable all the way down, so nothing can change behind a cached result. */
pub(crate) fn deeply_immutable(v: Val, heap: &HeapPool, depth: usize) -> bool {
    if !v.is_heap() { return true; }
    // Post-call args aren't rooted, so the body may have freed one, a freed slot (None) is not memoizable.
    match heap.try_get(v) {
        Some(HeapObj::Str(_) | HeapObj::Bytes(_) | HeapObj::LongInt(_) | HeapObj::Range(..) | HeapObj::NativeFn(_) | HeapObj::Type(_)) => true,
        Some(HeapObj::Tuple(items)) => depth < EQ_DEPTH_MAX && items.iter().all(|&x| deeply_immutable(x, heap, depth + 1)),
        Some(HeapObj::FrozenSet(items)) => depth < EQ_DEPTH_MAX && items.iter().all(|&x| deeply_immutable(x, heap, depth + 1)),
        _ => false,
    }
}

/* Disable a fi's memo after this many consecutive lookup misses, the scan tax outweighs stale hope. */
const MISS_LIMIT: u64 = 256;

/* `meta` holds SEEN first-run marks, then the consecutive misses of each fi. */
const SEEN: usize = 32;

// Indexed by dense `fi`, Vec gives O(1) lookup with no HashMap monomorphization.
pub struct Templates { slots: Vec<Vec<TplEntry>>, meta: Vec<u64> }

impl Templates {
    pub fn new() -> Self { Self { slots: Vec::new(), meta: Vec::new() } }

    pub fn clear(&mut self) { *self = Self::new(); }

    fn dead(&self, fi: usize) -> bool {
        self.meta.get(SEEN + fi).is_some_and(|&m| m >= MISS_LIMIT)
    }

    pub fn lookup(&mut self, fi: usize, args: &[Val], owner: Val, heap: &HeapPool) -> Option<Val> {
        let entries = self.slots.get(fi)?;
        if entries.is_empty() || self.dead(fi) { return None; }
        let h = hash_args(args, heap);
        let hit = entries.iter()
            .find(|e| args_match(e, args, owner, h, heap))
            .map(|e| e.result);
        if self.meta.len() <= SEEN + fi { self.meta.resize(SEEN + fi + 1, 0); }
        match hit {
            Some(_) => self.meta[SEEN + fi] = 0,
            None => {
                self.meta[SEEN + fi] += 1;
                // Reclaim the dead table, entries would otherwise stay GC roots forever.
                if self.meta[SEEN + fi] >= MISS_LIMIT { self.slots[fi] = Vec::new(); }
            }
        }
        hit
    }

    /* The key hash on its second run, so a key that never repeats allocates nothing. */
    pub fn admit(&mut self, fi: usize, args: &[Val], owner: Val, heap: &HeapPool) -> Option<u64> {
        if self.dead(fi) || self.slots.get(fi).is_some_and(|v| v.len() >= 256) { return None; }
        let h = hash_args(args, heap);
        // Fibonacci hashing, so keys apart only in high bits like `0.0` and `-0.0` land apart.
        let mark = (h ^ owner.0 ^ fi as u64).wrapping_mul(0x9e3779b97f4a7c15);
        if self.meta.len() < SEEN { self.meta.resize(SEEN, 0); }
        let seen = &mut self.meta[(mark >> (64 - SEEN.ilog2())) as usize];
        if *seen != mark { *seen = mark; return None; }
        Some(h)
    }

    pub fn holds(&self, fi: usize) -> bool { self.slots.get(fi).is_some_and(|v| !v.is_empty()) }

    pub fn insert(&mut self, fi: usize, args: &[Val], owner: Val, result: Val, h: u64) {
        if self.slots.len() <= fi { self.slots.resize_with(fi + 1, Vec::new); }
        self.slots[fi].push(TplEntry { args: args.to_vec(), owner, result, hash: h });
    }

    pub fn mark_all(&self, heap: &mut HeapPool) {
        for slot in &self.slots {
            for e in slot {
                for &v in &e.args { heap.mark(v); }
                heap.mark(e.owner);
                heap.mark(e.result);
            }
        }
    }
}

/* Fuse LoadAttr + [single-push arg loads] + Call into CallMethod+CallMethodArgs. Arg loads shift left one slot so the pair sits adjacent at the Call. Only pure single-push opcodes relocate, and never across a jump target. */
fn fuse_method_calls(chunk: &SSAChunk) -> Vec<Instruction> {
    let src = &chunk.instructions;
    let n = src.len();
    let mut out = src.clone();

    // Instruction indices any jump/handler/unwind can enter, relocation across them is unsafe.
    let mut targeted = vec![false; n + 1];
    for (k, ins) in src.iter().enumerate() {
        match ins.opcode {
            op if op.is_jump() => {
                let t = ins.operand as usize;
                if t <= n { targeted[t] = true; }
            }
            // Unwind::Goto resumes at the instruction after UnwindFinally.
            OpCode::UnwindFinally => targeted[k + 1] = true,
            _ => {}
        }
    }

    const MAX_WINDOW: usize = 8;
    let mut i = 0;
    while i + 1 < n {
        if src[i].opcode != OpCode::LoadAttr { i += 1; continue; }
        // Scan the run of relocatable single-push arg loads after the LoadAttr.
        let mut j = i + 1;
        while j < n
            && j - i - 1 < MAX_WINDOW
            && !targeted[j]
            && matches!(src[j].opcode, OpCode::LoadConst | OpCode::LoadName | OpCode::LoadTrue | OpCode::LoadFalse | OpCode::LoadNone)
        {
            j += 1;
        }
        if j >= n || src[j].opcode != OpCode::Call || targeted[j] { i += 1; continue; }
        // Every arg must be exactly one allowed push, else stack layout breaks.
        let raw = src[j].operand as usize;
        if (raw & 0xFF) + 2 * ((raw >> 8) & 0xFF) != j - i - 1 { i += 1; continue; }
        out[i..(j - 1)].copy_from_slice(&src[(i + 1)..j]);
        out[j - 1] = Instruction { opcode: OpCode::CallMethod, operand: src[i].operand };
        out[j] = Instruction { opcode: OpCode::CallMethodArgs, operand: src[j].operand };
        i = j + 1;
    }
    out
}
