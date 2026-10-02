use super::prelude::*;
use core::hash::{Hash, Hasher};

// `object.__hash__(x)` or `x.__hash__()`, the identity hash `hash()` gives a plain instance.
pub fn hash(vm: &mut VM, recv: Val, pos: &[Val]) -> Result<(), VmErr> {
    let mut h = crate::util::hash::FxHasher::default();
    pos.first().copied().unwrap_or(recv).0.hash(&mut h);
    vm.push(Val::int(h.finish() as i64 & Val::INT_MAX)); Ok(())
}
