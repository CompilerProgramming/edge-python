use crate::s;

use super::super::VM;
use super::super::types::*;

impl<'a> VM<'a> {

    /* `bytes_fromhex(s)`, the `bytes.fromhex` classmethod as a function. */
    pub fn call_bytes_fromhex(&mut self) -> Result<(), VmErr> {
        let s = self.pop()?;
        crate::vm::methods::bytes::fromhex(self, Val::none(), &[s])
    }

    /* `int_from_bytes(b, byteorder)`, the `int.from_bytes` classmethod as a function. */
    pub fn call_int_from_bytes(&mut self) -> Result<(), VmErr> {
        let order = self.pop()?;
        let b = self.pop()?;
        if !matches!(self.heap.try_get(b), Some(HeapObj::Bytes(_))) { return Err(cold_type("int_from_bytes() first arg must be bytes")); }
        crate::vm::methods::numeric::from_bytes(self, Val::none(), &[b, order])
    }

    // `int_to_bytes(n, length, byteorder)`, the `int.to_bytes` method as a function.
    pub fn call_int_to_bytes(&mut self) -> Result<(), VmErr> {
        let args = self.pop_n(3)?;
        crate::vm::methods::numeric::to_bytes(self, args[0], &args[1..])
    }

    // `import_module(name)`, fetch an already-imported module by alias, returns its `HeapObj::Module` Val.
    pub fn call_import_module(&mut self) -> Result<(), VmErr> {
        let spec = self.pop()?;
        let name = self.str_of(spec, "import_module() argument must be a string")?;
        // A name read at run time may be a builtin the program never wrote, give it its slot first.
        self.register_builtin(&name);
        let val = self.global(&name)
            .ok_or_else(|| VmErr::Raised(s!("NameError: module '", str &name, "' not imported in this scope")))?;
        if !matches!(self.heap.try_get(val), Some(HeapObj::Module(..))) {
            return Err(VmErr::TypeMsg(s!("'", str &name, "' is not a module")));
        }
        self.push(val); Ok(())
    }
}
