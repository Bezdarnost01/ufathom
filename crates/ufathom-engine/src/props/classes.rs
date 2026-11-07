//! Telling what an object is by walking its class up the super chain.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use ufathom_mem::{Addr, MemoryReader, MemoryReaderExt};
use ufathom_model::EngineLayout;

use crate::error::EngineError;
use crate::header::read_header;
use crate::limits::MAX_CHAIN_LENGTH;
use crate::names::NameTable;

/// Locks a mutex, ignoring poisoning: the guarded maps hold only memoized
/// answers, which stay valid after a panic elsewhere.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Answers "does this class derive from the class named X", memoized per
/// class address.
#[derive(Debug)]
struct ClassMatcher {
    target: &'static str,
    memo: Mutex<HashMap<Addr, bool>>,
}

impl ClassMatcher {
    fn new(target: &'static str) -> Self {
        Self {
            target,
            memo: Mutex::new(HashMap::new()),
        }
    }

    fn known(&self, class: Addr) -> Option<bool> {
        lock(&self.memo).get(&class).copied()
    }

    fn remember(&self, classes: &[Addr], answer: bool) {
        let mut memo = lock(&self.memo);
        for &class in classes {
            memo.insert(class, answer);
        }
    }
}

/// Class-chain questions about objects, shared by both property systems.
pub(crate) struct ObjectChains<'a> {
    mem: &'a dyn MemoryReader,
    layout: &'a EngineLayout,
    names: &'a NameTable,
    property: ClassMatcher,
    function: ClassMatcher,
    class_names: Mutex<HashMap<Addr, String>>,
}

impl<'a> ObjectChains<'a> {
    pub(crate) fn new(
        mem: &'a dyn MemoryReader,
        layout: &'a EngineLayout,
        names: &'a NameTable,
    ) -> Self {
        Self {
            mem,
            layout,
            names,
            property: ClassMatcher::new("Property"),
            function: ClassMatcher::new("Function"),
            class_names: Mutex::new(HashMap::new()),
        }
    }

    /// The class pointer of `object`, which must not be null.
    fn class_of(&self, object: Addr) -> Result<Addr, EngineError> {
        let at = object.wrapping_add(u64::from(self.layout.uobject.class));
        let class = self
            .mem
            .read_u64(at)
            .map_err(EngineError::read(at, "object class"))?;
        if class == 0 {
            return Err(EngineError::Null {
                addr: at,
                what: "object class",
            });
        }
        Ok(class)
    }

    /// Whether `object` is a `UProperty` (a class that derives from `Property`).
    pub(crate) fn is_property(&self, object: Addr) -> Result<bool, EngineError> {
        self.reaches(&self.property, self.class_of(object)?)
    }

    /// Whether `object` is a `UFunction` (a class that derives from `Function`).
    pub(crate) fn is_function(&self, object: Addr) -> Result<bool, EngineError> {
        self.reaches(&self.function, self.class_of(object)?)
    }

    fn reaches(&self, matcher: &ClassMatcher, start: Addr) -> Result<bool, EngineError> {
        let mut path: Vec<Addr> = Vec::new();
        let mut current = start;
        let answer = loop {
            if current == 0 {
                break false;
            }
            if let Some(known) = matcher.known(current) {
                break known;
            }
            if path.contains(&current) {
                return Err(EngineError::Cycle {
                    addr: current,
                    what: "class chain",
                });
            }
            if path.len() >= MAX_CHAIN_LENGTH {
                return Err(EngineError::Limit {
                    addr: current,
                    what: "class chain length",
                    value: path.len() as u64 + 1,
                    limit: MAX_CHAIN_LENGTH as u64,
                });
            }
            path.push(current);
            let header = read_header(self.mem, &self.layout.uobject, &self.layout.fname, current)?;
            // A class whose name does not resolve cannot match; its supers still can.
            let matches = self
                .names
                .resolve(&self.layout.fname, header.name.as_slice())
                .is_ok_and(|name| name.eq_ignore_ascii_case(matcher.target));
            if matches {
                break true;
            }
            let at = current.wrapping_add(u64::from(self.layout.ustruct.super_struct));
            current = self
                .mem
                .read_u64(at)
                .map_err(EngineError::read(at, "SuperStruct"))?;
        };
        matcher.remember(&path, answer);
        Ok(answer)
    }

    /// The name of the class object `class`, memoized.
    pub(crate) fn class_name(&self, class: Addr) -> Result<String, EngineError> {
        if let Some(name) = lock(&self.class_names).get(&class) {
            return Ok(name.clone());
        }
        let header = read_header(self.mem, &self.layout.uobject, &self.layout.fname, class)?;
        let name = self
            .names
            .resolve(&self.layout.fname, header.name.as_slice())
            .map_err(|error| error.at(class, "class name"))?;
        lock(&self.class_names).insert(class, name.clone());
        Ok(name)
    }
}

impl std::fmt::Debug for ObjectChains<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectChains").finish_non_exhaustive()
    }
}
