//! `UObject` headers and object paths.

use ufathom_mem::{Addr, MemoryReader};
use ufathom_model::layout::{FNameLayout, UObjectLayout};

use crate::error::EngineError;
use crate::limits::{MAX_FNAME_SIZE, MAX_PATH_DEPTH};
use crate::names::NameTable;
use crate::window::Window;

/// The bytes of one `FName`, as stored in an object.
///
/// Kept raw so the caller decides when to resolve it, and against which
/// [`NameTable`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FNameBytes {
    bytes: [u8; MAX_FNAME_SIZE],
    len: u8,
}

impl FNameBytes {
    /// Copies `bytes`.
    ///
    /// # Errors
    ///
    /// [`EngineError::Malformed`] when `bytes` is longer than
    /// [`MAX_FNAME_SIZE`].
    pub fn from_slice(bytes: &[u8]) -> Result<Self, EngineError> {
        let mut stored = [0u8; MAX_FNAME_SIZE];
        let target = stored
            .get_mut(..bytes.len())
            .ok_or_else(|| EngineError::Malformed {
                what: "FName bytes",
                reason: format!("{} bytes is above {MAX_FNAME_SIZE}", bytes.len()),
            })?;
        target.copy_from_slice(bytes);
        Ok(Self {
            bytes: stored,
            len: bytes.len() as u8,
        })
    }

    /// The stored bytes.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        self.bytes.get(..usize::from(self.len)).unwrap_or_default()
    }
}

/// The fixed fields of a `UObject`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ObjectHeader {
    /// Address of the object.
    pub addr: Addr,
    /// `EObjectFlags`.
    pub flags: u32,
    /// `InternalIndex`, the slot in the global object array.
    pub index: u32,
    /// `ClassPrivate`, zero when null.
    pub class: Addr,
    /// `NamePrivate`, unresolved.
    pub name: FNameBytes,
    /// `OuterPrivate`, zero when null.
    pub outer: Addr,
}

impl ObjectHeader {
    /// Checks that the object stores the slot it was found in.
    ///
    /// A zeroed header or memory reused by another object fails this check.
    ///
    /// # Errors
    ///
    /// [`EngineError::Inconsistent`] when the stored index differs.
    pub fn expect_index(&self, slot: u32) -> Result<(), EngineError> {
        if self.index == slot {
            Ok(())
        } else {
            Err(EngineError::Inconsistent {
                addr: self.addr,
                what: "object header",
                reason: format!(
                    "slot {slot} holds an object that stores index {}",
                    self.index
                ),
            })
        }
    }
}

/// Reads the header of the object at `addr`.
///
/// Nothing is validated beyond readability: a zeroed object reads as a
/// header of zeros. See [`ObjectHeader::expect_index`] and [`object_path`].
///
/// # Errors
///
/// [`EngineError::InvalidLayout`] for an `FName` size outside 1 to
/// [`MAX_FNAME_SIZE`], [`EngineError::Read`] for unreadable memory.
pub fn read_header(
    mem: &dyn MemoryReader,
    uobject: &UObjectLayout,
    fname: &FNameLayout,
    addr: Addr,
) -> Result<ObjectHeader, EngineError> {
    let size = fname.size as usize;
    if size == 0 || size > MAX_FNAME_SIZE {
        return Err(EngineError::InvalidLayout {
            what: "FName",
            reason: format!("size {size} is outside 1 to {MAX_FNAME_SIZE}"),
        });
    }
    let window = Window::read(
        mem,
        addr,
        "object header",
        &[
            (uobject.flags, 4),
            (uobject.index, 4),
            (uobject.class, 8),
            (uobject.name, fname.size),
            (uobject.outer, 8),
        ],
    )?;
    Ok(ObjectHeader {
        addr,
        flags: window.u32(uobject.flags)?,
        index: window.u32(uobject.index)?,
        class: window.u64(uobject.class)?,
        name: FNameBytes::from_slice(window.bytes(uobject.name, fname.size)?)?,
        outer: window.u64(uobject.outer)?,
    })
}

/// Builds the full path of the object at `addr`, such as
/// `/Script/Engine.Actor:K2_DestroyActor`.
///
/// The outer chain is followed to the top. The delimiter is `.` after a
/// package and `:` after an object directly inside a package, then `.` again,
/// which is what the engine prints. The walk is bounded by
/// [`MAX_PATH_DEPTH`] and stops at a cycle.
///
/// Every object on the chain must have a non-null class, since the engine
/// never creates one without. This is what makes a zeroed header an error
/// here and not the name `None`.
///
/// # Errors
///
/// [`EngineError::Read`] for an unreadable link, [`EngineError::Null`] for an
/// object without a class, [`EngineError::UnresolvedName`] (wrapped with the
/// object's address) for a name that does not resolve,
/// [`EngineError::Cycle`] and [`EngineError::Limit`] for a bad chain.
pub fn object_path(
    mem: &dyn MemoryReader,
    names: &NameTable,
    uobject: &UObjectLayout,
    fname: &FNameLayout,
    addr: Addr,
) -> Result<String, EngineError> {
    let mut chain: Vec<String> = Vec::new();
    let mut visited: Vec<Addr> = Vec::new();
    let mut current = addr;
    while current != 0 {
        if visited.contains(&current) {
            return Err(EngineError::Cycle {
                addr: current,
                what: "outer chain",
            });
        }
        if visited.len() >= MAX_PATH_DEPTH {
            return Err(EngineError::Limit {
                addr: current,
                what: "outer chain depth",
                value: visited.len() as u64 + 1,
                limit: MAX_PATH_DEPTH as u64,
            });
        }
        visited.push(current);
        let header = read_header(mem, uobject, fname, current)?;
        if header.class == 0 {
            return Err(EngineError::Null {
                addr: current,
                what: "object class",
            });
        }
        let name = names
            .resolve(fname, header.name.as_slice())
            .map_err(|error| error.at(current, "object name"))?;
        chain.push(name);
        current = header.outer;
    }
    let mut path = String::new();
    for (depth, name) in chain.iter().rev().enumerate() {
        match depth {
            0 => {}
            // Child of a package: `Package.Object`.
            1 => path.push('.'),
            // Child of an object that is directly in a package: `Object:Member`.
            2 => path.push(':'),
            _ => path.push('.'),
        }
        path.push_str(name);
    }
    Ok(path)
}
