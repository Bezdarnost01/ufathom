//! [`Session`]: names, objects and lookups over any reader.

use std::collections::HashMap;
use std::ops::ControlFlow;
use std::sync::{Arc, Mutex, PoisonError};

use ufathom_mem::{MemError, MemoryReader, MemoryReaderExt, Pod};

use crate::containers::{FString, TArray};
use crate::desc::{EngineDesc, NameArrayDesc, NamePoolDesc, NamesDesc, Place};
use crate::object::{ObjectRef, UObjectBase};
use crate::ptr::Ptr;
use crate::traits::TypePath;

/// Longest name read, in characters.
const MAX_NAME_CHARS: usize = 1024;
/// Most blocks an `FNamePool` can have.
const MAX_NAME_BLOCKS: u32 = 8192;
/// Deepest outer chain or class hierarchy followed.
const MAX_CHAIN: usize = 256;
/// Items read per request when walking a flat object array.
const FLAT_BATCH: u32 = 4096;
/// Most objects an array may claim.
const MAX_OBJECTS: u32 = 1 << 26;
/// `RF_ClassDefaultObject`.
const RF_CLASS_DEFAULT_OBJECT: u32 = 0x10;

/// Why a [`Session`] could not answer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// Memory could not be read.
    #[error("cannot read {what} at {addr:#x}: {source}")]
    Read {
        /// What was being read.
        what: &'static str,
        /// Where.
        addr: u64,
        /// The reader's error.
        #[source]
        source: MemError,
    },
    /// The module a [`Place`] refers to is not loaded.
    #[error("module {0} is not loaded")]
    ModuleNotFound(String),
    /// The data read makes no sense for this build.
    #[error("{what} at {addr:#x}: {reason}")]
    Invalid {
        /// What was being read.
        what: &'static str,
        /// Where.
        addr: u64,
        /// What is wrong with it.
        reason: String,
    },
    /// The generated description does not know this value for this build.
    #[error("{0} was not found when the SDK was generated")]
    Unknown(&'static str),
}

/// Result of a [`Session`] call.
pub type SessionResult<T> = Result<T, SessionError>;

/// The fields of an object header a session uses.
struct Header {
    flags: u32,
    index: u32,
    class: u64,
    outer: u64,
    name: [u8; 16],
}

/// Names and objects of one engine build, read through `M`.
///
/// This is the in-game toolbox of an SDK: `FName` to text, object paths,
/// lookups by path or by type, `IsA`, the class default object, `GWorld` and
/// `GEngine`. Every read is checked by the reader, so a stale or wrong pointer
/// is an error, never a fault. With [`SelfMemory`](crate::mem::SelfMemory)
/// it works inside the game; with `ProcessMemory` it works from another
/// process, read-only.
///
/// Name text is cached by entry index, since entries never change once
/// written, and path lookups are cached and re-validated on every hit.
pub struct Session<M> {
    mem: M,
    desc: &'static EngineDesc,
    objects_at: u64,
    names_at: u64,
    header_len: usize,
    names: Mutex<HashMap<u32, Arc<str>>>,
    found: Mutex<HashMap<String, Ptr<UObjectBase>>>,
    by_name: Mutex<Option<NameIndex>>,
}

type NameIndex = HashMap<String, Vec<Ptr<UObjectBase>>>;

impl<M: MemoryReader> Session<M> {
    /// Opens a session for the build described by `desc`.
    ///
    /// # Errors
    ///
    /// [`SessionError::ModuleNotFound`] when a module named by `desc` is not
    /// loaded, [`SessionError::Read`] when an indirect name table pointer
    /// cannot be read, [`SessionError::Invalid`] for an unusable description.
    pub fn new(mem: M, desc: &'static EngineDesc) -> SessionResult<Self> {
        let objects_at = resolve(&mem, &desc.objects.location)?;
        let names_at = match &desc.names {
            NamesDesc::Pool(pool) => resolve(&mem, &pool.location)?,
            NamesDesc::Array(array) => {
                let at = resolve(&mem, &array.location)?;
                if array.indirect {
                    let table = read_u64(&mem, at, "name array pointer")?;
                    if table == 0 {
                        return Err(invalid("name array pointer", at, "null"));
                    }
                    table
                } else {
                    at
                }
            }
        };
        if let NamesDesc::Pool(pool) = &desc.names
            && (pool.stride == 0 || pool.block_offset_bits == 0 || pool.block_offset_bits > 31)
        {
            return Err(invalid(
                "name pool description",
                names_at,
                "bad stride or bits",
            ));
        }
        if let NamesDesc::Array(array) = &desc.names
            && array.elements_per_chunk == 0
        {
            return Err(invalid("name array description", names_at, "empty chunks"));
        }
        if desc.objects.item_size == 0 || desc.objects.elements_per_chunk == Some(0) {
            return Err(invalid(
                "object array description",
                objects_at,
                "empty items",
            ));
        }
        let o = &desc.object;
        let header_len = [
            o.flags + 4,
            o.index + 4,
            o.class + 8,
            o.outer + 8,
            o.name + desc.fname.size.min(16),
        ]
        .into_iter()
        .max()
        .unwrap_or(0) as usize;
        Ok(Self {
            mem,
            desc,
            objects_at,
            names_at,
            header_len,
            names: Mutex::new(HashMap::new()),
            found: Mutex::new(HashMap::new()),
            by_name: Mutex::new(None),
        })
    }

    /// The reader.
    pub fn memory(&self) -> &M {
        &self.mem
    }

    /// The build description.
    #[must_use]
    pub fn desc(&self) -> &'static EngineDesc {
        self.desc
    }

    /// Reads a [`Pod`] value at `ptr`.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`].
    pub fn read<T: Pod>(&self, ptr: Ptr<T>) -> SessionResult<T> {
        self.mem
            .read_pod(ptr.addr())
            .map_err(read_error(ptr.addr(), "value"))
    }

    /// Checked field access to the object at `ptr`.
    pub fn object<T>(&self, ptr: Ptr<T>) -> ObjectRef<'_, T> {
        ObjectRef::new(&self.mem, ptr.addr())
    }

    // ---- the object array ----

    /// Number of slots in use in the object array.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`], or [`SessionError::Invalid`] for an absurd count.
    pub fn num_objects(&self) -> SessionResult<u32> {
        let at = self
            .objects_at
            .wrapping_add(u64::from(self.desc.objects.num_elements));
        let num = self
            .mem
            .read_u32(at)
            .map_err(read_error(at, "object count"))?;
        if num > MAX_OBJECTS {
            return Err(invalid("object count", at, format!("{num} objects")));
        }
        Ok(num)
    }

    /// The object in slot `index`, `None` for an empty or out of range slot.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`] or [`SessionError::Invalid`].
    pub fn object_at(&self, index: u32) -> SessionResult<Option<Ptr<UObjectBase>>> {
        if index >= self.num_objects()? {
            return Ok(None);
        }
        let item = self.item_addr(index)?;
        let at = item.wrapping_add(u64::from(self.desc.objects.item_object));
        let object = self
            .mem
            .read_u64(at)
            .map_err(read_error(at, "object pointer"))?;
        Ok((object != 0).then(|| Ptr::new(object)))
    }

    /// The serial number of slot `index`, when the build has one.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`] or [`SessionError::Invalid`].
    pub fn serial_at(&self, index: u32) -> SessionResult<Option<u32>> {
        let Some(offset) = self.desc.objects.item_serial else {
            return Ok(None);
        };
        let at = self.item_addr(index)?.wrapping_add(u64::from(offset));
        self.mem
            .read_u32(at)
            .map(Some)
            .map_err(read_error(at, "object serial"))
    }

    /// Calls `visit` with the index and address of every live object, in
    /// slot order, until it returns [`ControlFlow::Break`].
    ///
    /// Items are read a chunk at a time, so a walk costs a few reads per chunk
    /// plus what `visit` reads.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`] or [`SessionError::Invalid`] for the array
    /// itself. Errors inside `visit` are its own business.
    pub fn for_each_object(
        &self,
        mut visit: impl FnMut(u32, Ptr<UObjectBase>) -> ControlFlow<()>,
    ) -> SessionResult<()> {
        let num = self.num_objects()?;
        let batch = self.desc.objects.elements_per_chunk.unwrap_or(FLAT_BATCH);
        let item_size = self.desc.objects.item_size as usize;
        let item_object = self.desc.objects.item_object as usize;
        let mut start = 0u32;
        while start < num {
            let count = batch.min(num - start);
            let first = self.item_addr(start)?;
            let mut bytes = vec![0u8; count as usize * item_size];
            self.mem
                .read_exact(first, &mut bytes)
                .map_err(read_error(first, "object items"))?;
            for (n, item) in bytes.chunks_exact(item_size).enumerate() {
                let object = field_u64(item, item_object);
                if object != 0
                    && visit(start + n as u32, Ptr::new(object)) == ControlFlow::Break(())
                {
                    return Ok(());
                }
            }
            start += count;
        }
        Ok(())
    }

    /// Every live object, in slot order.
    ///
    /// # Errors
    ///
    /// As [`for_each_object`](Self::for_each_object).
    pub fn objects(&self) -> SessionResult<Vec<Ptr<UObjectBase>>> {
        let mut all = Vec::new();
        self.for_each_object(|_, object| {
            all.push(object);
            ControlFlow::Continue(())
        })?;
        Ok(all)
    }

    /// `true` when `object` is non-null, readable and still sits in the slot
    /// its `InternalIndex` names. A freed object whose slot was reused fails
    /// this check.
    pub fn is_valid<T>(&self, object: Ptr<T>) -> bool {
        if object.is_null() {
            return false;
        }
        let Ok(header) = self.header(object.addr()) else {
            return false;
        };
        matches!(self.object_at(header.index), Ok(Some(slot)) if slot.addr() == object.addr())
    }

    // ---- one object ----

    /// The `InternalIndex` of `object`.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`].
    pub fn index_of<T>(&self, object: Ptr<T>) -> SessionResult<u32> {
        Ok(self.header(object.addr())?.index)
    }

    /// The `ObjectFlags` of `object`.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`].
    pub fn flags_of<T>(&self, object: Ptr<T>) -> SessionResult<u32> {
        Ok(self.header(object.addr())?.flags)
    }

    /// The class of `object`.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`], or [`SessionError::Invalid`] for a null class.
    pub fn class_of<T>(&self, object: Ptr<T>) -> SessionResult<Ptr<UObjectBase>> {
        let class = self.header(object.addr())?.class;
        if class == 0 {
            return Err(invalid("object class", object.addr(), "null"));
        }
        Ok(Ptr::new(class))
    }

    /// The outer of `object`, `None` for a package.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`].
    pub fn outer_of<T>(&self, object: Ptr<T>) -> SessionResult<Option<Ptr<UObjectBase>>> {
        let outer = self.header(object.addr())?.outer;
        Ok((outer != 0).then(|| Ptr::new(outer)))
    }

    /// The parent of a class or struct, `None` for a root.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`].
    pub fn super_of<T>(&self, structure: Ptr<T>) -> SessionResult<Option<Ptr<UObjectBase>>> {
        let at = structure
            .addr()
            .wrapping_add(u64::from(self.desc.object.super_struct));
        let parent = self
            .mem
            .read_u64(at)
            .map_err(read_error(at, "super struct"))?;
        Ok((parent != 0).then(|| Ptr::new(parent)))
    }

    /// The name of `object`, with its number: `Actor`, `Cube_3`.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`] or [`SessionError::Invalid`] for a name that does
    /// not resolve.
    pub fn name_of<T>(&self, object: Ptr<T>) -> SessionResult<String> {
        let header = self.header(object.addr())?;
        self.fname_to_string(&header.name)
    }

    /// The path of `object`: `/Script/Engine.Actor:K2_DestroyActor`, the form
    /// the dump and the generated wrappers use.
    ///
    /// # Errors
    ///
    /// As [`name_of`](Self::name_of), plus [`SessionError::Invalid`] for an
    /// outer chain that loops or is too deep.
    pub fn path_of<T>(&self, object: Ptr<T>) -> SessionResult<String> {
        let mut chain: Vec<String> = Vec::new();
        let mut current = object.addr();
        while current != 0 {
            if chain.len() >= MAX_CHAIN {
                return Err(invalid("outer chain", object.addr(), "too deep or a loop"));
            }
            let header = self.header(current)?;
            chain.push(self.fname_to_string(&header.name)?);
            current = header.outer;
        }
        let mut path = String::new();
        for (depth, name) in chain.iter().rev().enumerate() {
            match depth {
                0 => {}
                1 => path.push('.'),
                2 => path.push(':'),
                _ => path.push('.'),
            }
            path.push_str(name);
        }
        Ok(path)
    }

    /// `ClassName Path`, as `UObject::GetFullName` prints it.
    ///
    /// # Errors
    ///
    /// As [`path_of`](Self::path_of).
    pub fn full_name_of<T>(&self, object: Ptr<T>) -> SessionResult<String> {
        let class = self.class_of(object)?;
        Ok(format!(
            "{} {}",
            self.name_of(class)?,
            self.path_of(object)?
        ))
    }

    /// `true` when `object` is an instance of `class` or of a class derived
    /// from it.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`], or [`SessionError::Invalid`] for a class chain
    /// that is too deep.
    pub fn is_a<T, C>(&self, object: Ptr<T>, class: Ptr<C>) -> SessionResult<bool> {
        let mut current = Some(self.class_of(object)?);
        let mut depth = 0;
        while let Some(at) = current {
            if at.addr() == class.addr() {
                return Ok(true);
            }
            depth += 1;
            if depth > MAX_CHAIN {
                return Err(invalid("class chain", at.addr(), "too deep or a loop"));
            }
            current = self.super_of(at)?;
        }
        Ok(false)
    }

    /// The parameter block size a function declares (`PropertiesSize`).
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`], or [`SessionError::Invalid`] for a negative
    /// size.
    pub fn params_size<T>(&self, function: Ptr<T>) -> SessionResult<usize> {
        let at = function
            .addr()
            .wrapping_add(u64::from(self.desc.object.properties_size));
        let size = self
            .mem
            .read_i32(at)
            .map_err(read_error(at, "parameter size"))?;
        usize::try_from(size).map_err(|_| invalid("parameter size", at, format!("{size}")))
    }

    // ---- lookups ----

    /// Finds the object with this exact path, such as
    /// `/Script/Engine.Actor` or `/Script/Engine.Actor:K2_DestroyActor`.
    ///
    /// A hit is cached by path and re-validated on every use. Otherwise the
    /// candidates come from an index of every object by name, built in one
    /// walk of the object array; only when none of them matches is the array
    /// walked again, which also refreshes the index. So many first lookups
    /// cost one walk, and only a miss (an object that is not loaded) costs
    /// another.
    ///
    /// # Errors
    ///
    /// As [`for_each_object`](Self::for_each_object). Objects whose names do
    /// not resolve are skipped.
    pub fn find_object(&self, path: &str) -> SessionResult<Option<Ptr<UObjectBase>>> {
        let cached = lock(&self.found).get(path).copied();
        if let Some(hit) = cached {
            if self.is_valid(hit) && self.path_of(hit).is_ok_and(|p| p == path) {
                return Ok(Some(hit));
            }
            lock(&self.found).remove(path);
        }
        let last = path.rsplit(['.', ':']).next().unwrap_or(path);
        let matches = |object: Ptr<UObjectBase>| {
            self.is_valid(object) && self.path_of(object).is_ok_and(|p| p == path)
        };
        let result = self.search(last, matches)?;
        if let Some(found) = result {
            lock(&self.found).insert(path.to_owned(), found);
        }
        Ok(result)
    }

    /// Finds the first object, in slot order, named `name` (without its
    /// path), optionally only among instances of the class at `class_path`.
    /// Less precise than [`find_object`](Self::find_object); useful for
    /// exploring.
    ///
    /// # Errors
    ///
    /// As [`find_object`](Self::find_object).
    pub fn find_by_name(
        &self,
        name: &str,
        class_path: Option<&str>,
    ) -> SessionResult<Option<Ptr<UObjectBase>>> {
        let class = match class_path {
            Some(path) => match self.find_object(path)? {
                Some(class) => Some(class),
                None => return Ok(None),
            },
            None => None,
        };
        self.search(name, |object| {
            self.is_valid(object)
                && class.is_none_or(|class| self.is_a(object, class).unwrap_or(false))
        })
    }

    /// The first object named `name` that `accept` takes: from the name index
    /// when it has one, otherwise after rebuilding the index.
    fn search(
        &self,
        name: &str,
        accept: impl Fn(Ptr<UObjectBase>) -> bool,
    ) -> SessionResult<Option<Ptr<UObjectBase>>> {
        let known: Option<Vec<Ptr<UObjectBase>>> = lock(&self.by_name)
            .as_ref()
            .map(|index| index.get(name).cloned().unwrap_or_default());
        if let Some(candidates) = known
            && let Some(hit) = candidates.into_iter().find(|&c| accept(c))
        {
            return Ok(Some(hit));
        }
        let index = self.build_name_index()?;
        let hit = index
            .get(name)
            .and_then(|candidates| candidates.iter().copied().find(|&c| accept(c)));
        *lock(&self.by_name) = Some(index);
        Ok(hit)
    }

    /// Every live object by name, each list in slot order.
    fn build_name_index(&self) -> SessionResult<NameIndex> {
        let mut index = NameIndex::new();
        self.for_each_object(|_, object| {
            if let Ok(name) = self.name_of(object) {
                index.entry(name).or_default().push(object);
            }
            ControlFlow::Continue(())
        })?;
        Ok(index)
    }

    /// The class (or script struct) object of the generated type `T`:
    /// `T::StaticClass()`.
    ///
    /// # Errors
    ///
    /// As [`find_object`](Self::find_object), plus
    /// [`SessionError::Invalid`] when the type is not loaded.
    pub fn static_class<T: TypePath>(&self) -> SessionResult<Ptr<UObjectBase>> {
        self.find_object(T::PATH)?
            .ok_or_else(|| invalid("type", 0, format!("{} is not loaded", T::PATH)))
    }

    /// The class default object of `T`: `T::GetDefaultObj()`.
    ///
    /// # Errors
    ///
    /// As [`default_object_of`](Self::default_object_of).
    pub fn default_object<T: TypePath>(&self) -> SessionResult<Ptr<T>> {
        Ok(self.default_object_of(T::PATH)?.cast())
    }

    /// The class default object of the class at `class_path`.
    ///
    /// # Errors
    ///
    /// As [`find_object`](Self::find_object), plus
    /// [`SessionError::Unknown`] when the build's `ClassDefaultObject` offset
    /// was not found and [`SessionError::Invalid`] when the class is not
    /// loaded or its default object is null.
    pub fn default_object_of(&self, class_path: &str) -> SessionResult<Ptr<UObjectBase>> {
        let offset = self
            .desc
            .object
            .default_object
            .ok_or(SessionError::Unknown("UClass::ClassDefaultObject"))?;
        let class = self
            .find_object(class_path)?
            .ok_or_else(|| invalid("type", 0, format!("{class_path} is not loaded")))?;
        let at = class.addr().wrapping_add(u64::from(offset));
        let object = self
            .mem
            .read_u64(at)
            .map_err(read_error(at, "default object"))?;
        if object == 0 {
            return Err(invalid("default object", at, "null"));
        }
        Ok(Ptr::new(object))
    }

    // ---- values ----

    /// The text of an `FString`, for example a return value or a field.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`] for an inconsistent or unreadable string.
    pub fn fstring(&self, value: &FString) -> SessionResult<String> {
        value
            .read(&self.mem)
            .map_err(read_error(value.chars.data.addr(), "FString"))
    }

    /// The text of an `FName` value of this build, such as the SDK's
    /// `FName` alias.
    ///
    /// # Errors
    ///
    /// As [`fname_to_string`](Self::fname_to_string).
    pub fn fname_text<F: Pod>(&self, value: &F) -> SessionResult<String> {
        // SAFETY: `F: Pod` has no padding-dependent invariants; viewing its
        // bytes is a read of initialised memory of `size_of::<F>()` bytes.
        let bytes = unsafe {
            core::slice::from_raw_parts(core::ptr::from_ref(value).cast::<u8>(), size_of::<F>())
        };
        self.fname_to_string(bytes)
    }

    /// Every element of a `TArray`.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`] for an inconsistent or unreadable array.
    pub fn read_array<T: Pod>(&self, array: &TArray<T>) -> SessionResult<Vec<T>> {
        array
            .read_all(&self.mem)
            .map_err(read_error(array.data.addr(), "TArray"))
    }

    /// `object` as a `T` when it is an instance of `T` or a subclass.
    ///
    /// # Errors
    ///
    /// As [`static_class`](Self::static_class) and [`is_a`](Self::is_a).
    pub fn cast<T: TypePath, U>(&self, object: Ptr<U>) -> SessionResult<Option<Ptr<T>>> {
        if object.is_null() {
            return Ok(None);
        }
        let class = self.static_class::<T>()?;
        Ok(self.is_a(object, class)?.then(|| object.cast()))
    }

    /// Every live instance of `T` and its subclasses, class default objects
    /// excluded.
    ///
    /// # Errors
    ///
    /// As [`static_class`](Self::static_class) and
    /// [`for_each_object`](Self::for_each_object).
    pub fn objects_of<T: TypePath>(&self) -> SessionResult<Vec<Ptr<T>>> {
        let class = self.static_class::<T>()?;
        let mut all = Vec::new();
        self.for_each_object(|_, object| {
            if self
                .header(object.addr())
                .is_ok_and(|h| h.flags & RF_CLASS_DEFAULT_OBJECT == 0)
                && self.is_a(object, class).unwrap_or(false)
            {
                all.push(object.cast());
            }
            ControlFlow::Continue(())
        })?;
        Ok(all)
    }

    /// The live `UWorld` (`GWorld`), `None` while there is none.
    ///
    /// # Errors
    ///
    /// [`SessionError::Unknown`] when the slot was not found when the SDK was
    /// generated, otherwise as [`Session::new`].
    pub fn gworld(&self) -> SessionResult<Option<Ptr<UObjectBase>>> {
        self.global(self.desc.gworld.as_ref(), "GWorld")
    }

    /// The live `UEngine` (`GEngine`), `None` while there is none.
    ///
    /// # Errors
    ///
    /// As [`gworld`](Self::gworld).
    pub fn gengine(&self) -> SessionResult<Option<Ptr<UObjectBase>>> {
        self.global(self.desc.gengine.as_ref(), "GEngine")
    }

    fn global(
        &self,
        place: Option<&Place>,
        what: &'static str,
    ) -> SessionResult<Option<Ptr<UObjectBase>>> {
        let place = place.ok_or(SessionError::Unknown(what))?;
        let at = resolve(&self.mem, place)?;
        let object = self.mem.read_u64(at).map_err(read_error(at, what))?;
        Ok((object != 0).then(|| Ptr::new(object)))
    }

    // ---- names ----

    /// The text of an `FName` given as its bytes in memory, such as the
    /// `NamePrivate` of an object. A number above zero is appended as
    /// `_{Number-1}`, as the engine prints it.
    ///
    /// # Errors
    ///
    /// [`SessionError::Invalid`] when `bytes` is too short or no index
    /// resolves, [`SessionError::Read`] for an unreadable entry.
    pub fn fname_to_string(&self, bytes: &[u8]) -> SessionResult<String> {
        let f = &self.desc.fname;
        let get = |offset: u32| -> SessionResult<u32> {
            let at = offset as usize;
            bytes
                .get(at..at + 4)
                .and_then(|s| <[u8; 4]>::try_from(s).ok())
                .map(u32::from_le_bytes)
                .ok_or_else(|| invalid("FName bytes", 0, "too short"))
        };
        let comparison = get(f.comparison_index)?;
        let display = f.display_index.map(get).transpose()?;
        let number = f.number.map(get).transpose()?.unwrap_or(0);
        let text = match display.map(|index| self.entry_text(index)) {
            Some(Ok(text)) => text,
            _ => self.entry_text(comparison)?,
        };
        Ok(if number == 0 {
            text.to_string()
        } else {
            format!("{text}_{}", number - 1)
        })
    }

    /// The text of name table entry `index`, cached.
    ///
    /// # Errors
    ///
    /// [`SessionError::Read`] or [`SessionError::Invalid`].
    pub fn entry_text(&self, index: u32) -> SessionResult<Arc<str>> {
        if let Some(text) = lock(&self.names).get(&index) {
            return Ok(Arc::clone(text));
        }
        let text: Arc<str> = match &self.desc.names {
            NamesDesc::Pool(pool) => self.pool_entry(pool, index, true)?,
            NamesDesc::Array(array) => self.array_entry(array, index)?,
        }
        .into();
        lock(&self.names).insert(index, Arc::clone(&text));
        Ok(text)
    }

    fn pool_entry(&self, pool: &NamePoolDesc, index: u32, follow: bool) -> SessionResult<String> {
        let block = index >> pool.block_offset_bits;
        let offset =
            u64::from(index & ((1u32 << pool.block_offset_bits) - 1)) * u64::from(pool.stride);
        if block >= MAX_NAME_BLOCKS {
            return Err(invalid(
                "name index",
                0,
                format!("{index:#x} is past the pool"),
            ));
        }
        let slot = self
            .names_at
            .wrapping_add(u64::from(pool.blocks))
            .wrapping_add(u64::from(block) * 8);
        let block_ptr = read_u64(&self.mem, slot, "name block pointer")?;
        if block_ptr == 0 {
            return Err(invalid(
                "name block",
                slot,
                format!("block {block} is null"),
            ));
        }
        let entry = block_ptr.wrapping_add(offset);
        let header_at = entry.wrapping_add(u64::from(pool.header_offset));
        let header = self
            .mem
            .read_u16(header_at)
            .map_err(read_error(header_at, "name entry header"))?;
        let wide = u32::from(header) & pool.wide_mask != 0;
        let len = usize::from(header >> pool.len_shift);
        let chars_at = entry.wrapping_add(u64::from(pool.string_offset));
        if len == 0 {
            // Outline numbers: the entry is another entry's index and a number.
            if !follow || self.desc.fname.number.is_some() {
                return Err(invalid("name entry", entry, "empty"));
            }
            let target = self
                .mem
                .read_u32(chars_at)
                .map_err(read_error(chars_at, "numbered name"))?;
            let number_at = chars_at.wrapping_add(4);
            let number = self
                .mem
                .read_u32(number_at)
                .map_err(read_error(number_at, "numbered name"))?;
            let base = self.pool_entry(pool, target, false)?;
            return Ok(if number == 0 {
                base
            } else {
                format!("{base}_{}", number - 1)
            });
        }
        if len > MAX_NAME_CHARS {
            return Err(invalid("name entry", entry, format!("{len} characters")));
        }
        let mut bytes = vec![0u8; len * if wide { 2 } else { 1 }];
        self.mem
            .read_exact(chars_at, &mut bytes)
            .map_err(read_error(chars_at, "name characters"))?;
        Ok(decode(&bytes, wide))
    }

    fn array_entry(&self, array: &NameArrayDesc, index: u32) -> SessionResult<String> {
        let count_at = self.names_at.wrapping_add(u64::from(array.num_elements));
        let count = self
            .mem
            .read_u32(count_at)
            .map_err(read_error(count_at, "name count"))?;
        if index >= count {
            return Err(invalid("name index", 0, format!("{index} of {count}")));
        }
        let chunk_slot = self
            .names_at
            .wrapping_add(u64::from(index / array.elements_per_chunk) * 8);
        let chunk = read_u64(&self.mem, chunk_slot, "name chunk pointer")?;
        if chunk == 0 {
            return Err(invalid("name chunk", chunk_slot, "null"));
        }
        let entry_slot = chunk.wrapping_add(u64::from(index % array.elements_per_chunk) * 8);
        let entry = read_u64(&self.mem, entry_slot, "name entry pointer")?;
        if entry == 0 {
            return Err(invalid("name entry", entry_slot, "null"));
        }
        let field_at = entry.wrapping_add(u64::from(array.index_offset));
        let field = self
            .mem
            .read_u32(field_at)
            .map_err(read_error(field_at, "name entry index"))?;
        let chars_at = entry.wrapping_add(u64::from(array.string_offset));
        self.read_terminated(chars_at, field & array.wide_mask != 0)
    }

    /// Reads a NUL-terminated string of at most [`MAX_NAME_CHARS`] characters,
    /// in small steps so a terminator just before an unmapped page is found.
    fn read_terminated(&self, at: u64, wide: bool) -> SessionResult<String> {
        const STEP: usize = 64;
        let unit = if wide { 2 } else { 1 };
        let mut collected: Vec<u8> = Vec::new();
        let mut cursor = at;
        while collected.len() / unit <= MAX_NAME_CHARS {
            let mut chunk = vec![0u8; STEP * unit];
            let got = if self.mem.read_exact(cursor, &mut chunk).is_ok() {
                STEP
            } else {
                // Unit by unit up to the first unreadable one.
                let mut n = 0;
                while n < STEP {
                    let Some(part) = chunk.get_mut(n * unit..(n + 1) * unit) else {
                        break;
                    };
                    if self
                        .mem
                        .read_exact(cursor.wrapping_add((n * unit) as u64), part)
                        .is_err()
                    {
                        break;
                    }
                    n += 1;
                }
                n
            };
            for piece in chunk.chunks_exact(unit).take(got) {
                if piece.iter().all(|&b| b == 0) {
                    return Ok(decode(&collected, wide));
                }
                collected.extend_from_slice(piece);
            }
            if got < STEP {
                return Err(invalid(
                    "name characters",
                    at,
                    "no terminator before a hole",
                ));
            }
            cursor = cursor.wrapping_add((STEP * unit) as u64);
        }
        Err(invalid("name characters", at, "too long"))
    }

    // ---- helpers ----

    fn item_addr(&self, index: u32) -> SessionResult<u64> {
        let objects = &self.desc.objects;
        let stored_at = self.objects_at.wrapping_add(u64::from(objects.objects));
        let stored = self
            .mem
            .read_u64(stored_at)
            .map_err(read_error(stored_at, "object storage pointer"))?;
        let base = objects
            .pointer_transform
            .iter()
            .fold(stored, |value, step| step.apply(value));
        if base == 0 {
            return Err(invalid("object storage", stored_at, "null"));
        }
        let item_size = u64::from(objects.item_size);
        match objects.elements_per_chunk {
            None => Ok(base.wrapping_add(u64::from(index) * item_size)),
            Some(per) => {
                let slot = base.wrapping_add(u64::from(index / per) * 8);
                let chunk = read_u64(&self.mem, slot, "object chunk pointer")?;
                if chunk == 0 {
                    return Err(invalid("object chunk", slot, "null"));
                }
                Ok(chunk.wrapping_add(u64::from(index % per) * item_size))
            }
        }
    }

    fn header(&self, addr: u64) -> SessionResult<Header> {
        if addr == 0 {
            return Err(invalid("object", 0, "null"));
        }
        let mut bytes = vec![0u8; self.header_len];
        self.mem
            .read_exact(addr, &mut bytes)
            .map_err(read_error(addr, "object header"))?;
        let o = &self.desc.object;
        let mut name = [0u8; 16];
        let start = o.name as usize;
        let len = (self.desc.fname.size as usize).min(16);
        if let (Some(src), Some(dst)) = (bytes.get(start..start + len), name.get_mut(..len)) {
            dst.copy_from_slice(src);
        }
        Ok(Header {
            flags: field_u32(&bytes, o.flags as usize),
            index: field_u32(&bytes, o.index as usize),
            class: field_u64(&bytes, o.class as usize),
            outer: field_u64(&bytes, o.outer as usize),
            name,
        })
    }
}

impl<M> core::fmt::Debug for Session<M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Session")
            .field("objects_at", &format_args!("{:#x}", self.objects_at))
            .field("names_at", &format_args!("{:#x}", self.names_at))
            .finish_non_exhaustive()
    }
}

fn resolve(mem: &dyn MemoryReader, place: &Place) -> SessionResult<u64> {
    match place {
        Place::Absolute(addr) => Ok(*addr),
        Place::Rva { module, rva } => {
            let modules = mem.modules().map_err(read_error(0, "module list"))?;
            let wanted = file_name(module);
            modules
                .iter()
                .find(|m| file_name(&m.name).eq_ignore_ascii_case(wanted))
                .map(|m| m.base.wrapping_add(*rva))
                .ok_or_else(|| SessionError::ModuleNotFound((*module).to_owned()))
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

fn read_u64(mem: &dyn MemoryReader, at: u64, what: &'static str) -> SessionResult<u64> {
    mem.read_u64(at).map_err(read_error(at, what))
}

fn read_error(addr: u64, what: &'static str) -> impl FnOnce(MemError) -> SessionError {
    move |source| SessionError::Read { what, addr, source }
}

fn invalid(what: &'static str, addr: u64, reason: impl Into<String>) -> SessionError {
    SessionError::Invalid {
        what,
        addr,
        reason: reason.into(),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn field_u32(bytes: &[u8], at: usize) -> u32 {
    bytes
        .get(at..at + 4)
        .and_then(|s| <[u8; 4]>::try_from(s).ok())
        .map_or(0, u32::from_le_bytes)
}

fn field_u64(bytes: &[u8], at: usize) -> u64 {
    bytes
        .get(at..at + 8)
        .and_then(|s| <[u8; 8]>::try_from(s).ok())
        .map_or(0, u64::from_le_bytes)
}

fn decode(bytes: &[u8], wide: bool) -> String {
    if wide {
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| {
                u16::from_le_bytes([
                    pair.first().copied().unwrap_or(0),
                    pair.get(1).copied().unwrap_or(0),
                ])
            })
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        match std::str::from_utf8(bytes) {
            Ok(text) => text.to_owned(),
            Err(_) => bytes.iter().map(|&b| char::from(b)).collect(),
        }
    }
}
