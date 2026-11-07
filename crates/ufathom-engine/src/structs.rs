//! `UStruct`, `UFunction` and `UEnum` views.

use std::collections::HashSet;

use ufathom_mem::{Addr, MemoryReader, MemoryReaderExt};
use ufathom_model::layout::{FNameLayout, UEnumLayout, UFunctionLayout, UStructLayout};

use crate::error::EngineError;
use crate::header::FNameBytes;
use crate::limits::{MAX_CHAIN_LENGTH, MAX_ENUM_ENTRIES, MAX_FNAME_SIZE};
use crate::window::Window;

/// Low bits of a split enum array pointer that carry tags, not address.
const ENUM_POINTER_TAG_MASK: u64 = 0x7;

/// Width in bytes of a split enum array value.
const SPLIT_ENUM_VALUE_BYTES: u64 = 8;

/// Fields of a `UStruct` the walkers need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructView {
    /// Address of the struct.
    pub addr: Addr,
    /// `SuperStruct`, zero when null.
    pub super_struct: Addr,
    /// Head of the `Children` list, zero when empty.
    pub children: Addr,
    /// Head of the `ChildProperties` list. `None` when the layout has no such
    /// field, `Some(0)` when the list is empty.
    pub child_properties: Option<Addr>,
    /// `PropertiesSize`.
    pub properties_size: u32,
    /// `MinAlignment`, widened to 32 bits.
    pub min_alignment: u32,
}

/// Reads the `UStruct` fields of the object at `addr`.
///
/// `MinAlignment` is read with the width the layout states (16 bits from
/// UE 5.6).
///
/// # Errors
///
/// [`EngineError::InvalidLayout`] for an alignment width other than 1, 2, 4
/// or 8, [`EngineError::Read`] for unreadable memory.
pub fn read_struct(
    mem: &dyn MemoryReader,
    layout: &UStructLayout,
    addr: Addr,
) -> Result<StructView, EngineError> {
    let mut spans = vec![
        (layout.super_struct, 8),
        (layout.children, 8),
        (layout.properties_size, 4),
        (layout.min_alignment, layout.min_alignment_width),
    ];
    if let Some(child_properties) = layout.child_properties {
        spans.push((child_properties, 8));
    }
    let window = Window::read(mem, addr, "struct", &spans)?;
    let min_alignment = match layout.min_alignment_width {
        1 => u32::from(window.u8(layout.min_alignment)?),
        2 => u32::from(window.u16(layout.min_alignment)?),
        4 => window.u32(layout.min_alignment)?,
        8 => u32::try_from(window.u64(layout.min_alignment)?).unwrap_or(u32::MAX),
        other => {
            return Err(EngineError::InvalidLayout {
                what: "MinAlignment",
                reason: format!("width {other} is not 1, 2, 4 or 8"),
            });
        }
    };
    Ok(StructView {
        addr,
        super_struct: window.u64(layout.super_struct)?,
        children: window.u64(layout.children)?,
        child_properties: layout
            .child_properties
            .map(|offset| window.u64(offset))
            .transpose()?,
        properties_size: window.u32(layout.properties_size)?,
        min_alignment,
    })
}

/// Walks `SuperStruct` links from a struct upward.
///
/// Yields the struct itself first, then each super. Before a link is yielded
/// its own `SuperStruct` is read, so an unreadable link is reported as an
/// error item instead of being handed out. The walk stops after any error,
/// after [`MAX_CHAIN_LENGTH`] links, and at the first repeated struct, which
/// is reported as [`EngineError::Cycle`].
pub struct SuperChain<'a> {
    mem: &'a dyn MemoryReader,
    super_offset: u32,
    next: Option<Addr>,
    seen: HashSet<Addr>,
}

/// Starts a [`SuperChain`] at `addr`.
#[must_use]
pub fn super_chain<'a>(
    mem: &'a dyn MemoryReader,
    layout: &UStructLayout,
    addr: Addr,
) -> SuperChain<'a> {
    SuperChain {
        mem,
        super_offset: layout.super_struct,
        next: (addr != 0).then_some(addr),
        seen: HashSet::new(),
    }
}

impl Iterator for SuperChain<'_> {
    type Item = Result<Addr, EngineError>;

    fn next(&mut self) -> Option<Self::Item> {
        let current = self.next.take()?;
        if !self.seen.insert(current) {
            return Some(Err(EngineError::Cycle {
                addr: current,
                what: "super chain",
            }));
        }
        if self.seen.len() > MAX_CHAIN_LENGTH {
            return Some(Err(EngineError::Limit {
                addr: current,
                what: "super chain length",
                value: self.seen.len() as u64,
                limit: MAX_CHAIN_LENGTH as u64,
            }));
        }
        let at = current.wrapping_add(u64::from(self.super_offset));
        match self.mem.read_u64(at) {
            Ok(parent) => {
                self.next = (parent != 0).then_some(parent);
                Some(Ok(current))
            }
            Err(source) => Some(Err(EngineError::Read {
                addr: at,
                what: "SuperStruct",
                source,
            })),
        }
    }
}

/// Fields of a `UFunction`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionView {
    /// `EFunctionFlags`.
    pub flags: u32,
    /// `Func`, the exec entry point. `None` when the layout has no such field
    /// or the function is a script function with a null entry.
    pub native_func: Option<Addr>,
}

/// Reads the `UFunction` fields of the object at `addr`.
///
/// # Errors
///
/// [`EngineError::Read`] for unreadable memory.
pub fn read_function(
    mem: &dyn MemoryReader,
    layout: &UFunctionLayout,
    addr: Addr,
) -> Result<FunctionView, EngineError> {
    let mut spans = vec![(layout.flags, 4)];
    if let Some(native) = layout.native_func {
        spans.push((native, 8));
    }
    let window = Window::read(mem, addr, "function", &spans)?;
    let native_func = layout
        .native_func
        .map(|offset| window.u64(offset))
        .transpose()?
        .filter(|&func| func != 0);
    Ok(FunctionView {
        flags: window.u32(layout.flags)?,
        native_func,
    })
}

/// One enumerator as stored: the raw `FName` and the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumEntry {
    /// The name, unresolved. The engine stores it prefixed with the enum
    /// name, as `ENetRole::ROLE_None`.
    pub name: FNameBytes,
    /// The value. Values 2 bytes or wider are sign-extended, one-byte values
    /// are zero-extended.
    pub value: i64,
}

/// The part of an enumerator name after the enum prefix: `ROLE_None` for
/// `ENetRole::ROLE_None`. A name without a prefix is returned as is.
#[must_use]
pub fn short_enum_value_name(full: &str) -> &str {
    full.rsplit_once("::").map_or(full, |(_, short)| short)
}

/// Reads the enumerators of the `UEnum` at `addr`.
///
/// `PairArray` reads one array of `(FName, value)` pairs. `SplitArrays` reads
/// a names array and a values array of 8-byte values, masking the tag bits off
/// both pointers.
///
/// # Errors
///
/// - [`EngineError::Limit`] when the count is negative or above
///   [`MAX_ENUM_ENTRIES`]. This is checked before anything is allocated.
/// - [`EngineError::Null`] when the data pointer is null for a non-empty enum.
/// - [`EngineError::InvalidLayout`] for a stride or width that cannot hold the
///   fields.
/// - [`EngineError::Read`] for unreadable memory.
pub fn read_enum_entries(
    mem: &dyn MemoryReader,
    layout: &UEnumLayout,
    fname: &FNameLayout,
    addr: Addr,
) -> Result<Vec<EnumEntry>, EngineError> {
    let name_size = fname.size as usize;
    if name_size == 0 || name_size > MAX_FNAME_SIZE {
        return Err(EngineError::InvalidLayout {
            what: "FName",
            reason: format!("size {name_size} is outside 1 to {MAX_FNAME_SIZE}"),
        });
    }
    match *layout {
        UEnumLayout::PairArray {
            offset,
            stride,
            value_offset,
            value_width,
        } => {
            let count_at = offset
                .checked_add(8)
                .ok_or_else(|| EngineError::InvalidLayout {
                    what: "enum pair array",
                    reason: format!("offset {offset:#x} overflows"),
                })?;
            let window = Window::read(mem, addr, "enum names", &[(offset, 8), (count_at, 4)])?;
            let data = window.u64(offset)?;
            let count = window.u32(count_at)?;
            let count = checked_count(addr, count)?;
            if count == 0 {
                return Ok(Vec::new());
            }
            let fits = stride as usize >= name_size
                && value_offset
                    .checked_add(value_width)
                    .is_some_and(|end| end <= stride)
                && matches!(value_width, 1 | 2 | 4 | 8);
            if !fits {
                return Err(EngineError::InvalidLayout {
                    what: "enum pair array",
                    reason: format!(
                        "stride {stride:#x} cannot hold a name of {name_size} bytes and a value of {value_width} at {value_offset:#x}"
                    ),
                });
            }
            let bytes = read_array(mem, data, count, stride as usize, "enum entries")?;
            let mut entries = Vec::with_capacity(count);
            for pair in bytes.chunks_exact(stride as usize) {
                let name = pair.get(..name_size).unwrap_or_default();
                let raw = pair
                    .get(value_offset as usize..(value_offset + value_width) as usize)
                    .unwrap_or_default();
                entries.push(EnumEntry {
                    name: FNameBytes::from_slice(name)?,
                    value: widen(raw),
                });
            }
            Ok(entries)
        }
        UEnumLayout::SplitArrays {
            names,
            values,
            count,
        } => {
            let window = Window::read(
                mem,
                addr,
                "enum names",
                &[(names, 8), (values, 8), (count, 4)],
            )?;
            let count = checked_count(addr, window.u32(count)?)?;
            if count == 0 {
                return Ok(Vec::new());
            }
            let names_ptr = window.u64(names)? & !ENUM_POINTER_TAG_MASK;
            let values_ptr = window.u64(values)? & !ENUM_POINTER_TAG_MASK;
            let name_bytes = read_array(mem, names_ptr, count, name_size, "enum names array")?;
            let value_bytes = read_array(
                mem,
                values_ptr,
                count,
                SPLIT_ENUM_VALUE_BYTES as usize,
                "enum values array",
            )?;
            let mut entries = Vec::with_capacity(count);
            for (name, value) in name_bytes
                .chunks_exact(name_size)
                .zip(value_bytes.chunks_exact(SPLIT_ENUM_VALUE_BYTES as usize))
            {
                entries.push(EnumEntry {
                    name: FNameBytes::from_slice(name)?,
                    value: widen(value),
                });
            }
            Ok(entries)
        }
    }
}

/// Checks a `TArray` count read from the target before it sizes anything.
fn checked_count(addr: Addr, count: u32) -> Result<usize, EngineError> {
    let signed = count as i32;
    if signed < 0 || count as usize > MAX_ENUM_ENTRIES {
        return Err(EngineError::Limit {
            addr,
            what: "enum entry count",
            value: u64::from(count),
            limit: MAX_ENUM_ENTRIES as u64,
        });
    }
    Ok(count as usize)
}

fn read_array(
    mem: &dyn MemoryReader,
    data: Addr,
    count: usize,
    element: usize,
    what: &'static str,
) -> Result<Vec<u8>, EngineError> {
    if data == 0 {
        return Err(EngineError::Null { addr: data, what });
    }
    let mut bytes = vec![0u8; count * element];
    mem.read_exact(data, &mut bytes)
        .map_err(EngineError::read(data, what))?;
    Ok(bytes)
}

/// A one-byte value is unsigned, wider ones are signed.
fn widen(raw: &[u8]) -> i64 {
    match *raw {
        [a] => i64::from(a),
        [a, b] => i64::from(i16::from_le_bytes([a, b])),
        [a, b, c, d] => i64::from(i32::from_le_bytes([a, b, c, d])),
        [a, b, c, d, e, f, g, h] => i64::from_le_bytes([a, b, c, d, e, f, g, h]),
        _ => 0,
    }
}

impl std::fmt::Debug for SuperChain<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SuperChain")
            .field("next", &self.next)
            .finish_non_exhaustive()
    }
}
