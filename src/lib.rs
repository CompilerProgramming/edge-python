#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub mod abi;

/* Host bridge behind the six plugin imports, part of the wasm `runtime`. */
#[cfg(all(target_arch = "wasm32", feature = "runtime"))]
pub mod bridge;

#[cfg(all(target_arch = "wasm32", feature = "runtime"))]
pub mod wasm;

/* Internal compiler helpers (not Edge Python stdlib), separate from pipeline code. */
pub mod util {
    pub mod hash;
    pub mod fstr;
    pub mod jesc;
    pub mod sha256;
    pub mod uni;
}

/* NaN-boxed values and the mark-and-sweep heap, the layer both the frontend and the VM build on. */
pub mod value;

pub mod lexer;
pub mod parser;
/* Post-SSA passes, run between parse and boot, touches no VM state. */
pub mod optimizer;
pub mod vm;
pub mod modules;
