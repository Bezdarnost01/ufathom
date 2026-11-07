//! Step 4, first half: the fields of `UField` and `UStruct`.
//!
//! Every value comes from a relation the engine's class hierarchy must
//! satisfy, checked on the CoreUObject anchors:
//!
//! - `SuperStruct`: `Struct`'s super is `Field`, `Class`'s is `Struct`,
//!   `Field`'s is `Object`, and so on for all eight core classes.
//! - `PropertiesSize` and `MinAlignment`: `Guid` is 0x10 bytes with alignment
//!   4, `Color` is 4 with alignment 4, `Vector` is 0xC or 0x18, and
//!   `Transform` is aligned to 16.
//! - `Children`: the pointer whose target's outer is the owner.
//! - `Next`: walks the member list while the outer stays the owner.
//! - `ObjectFlags`: `Default__` objects carry the class-default-object bit,
//!   other objects do not.

use std::collections::HashSet;

use ufathom_model::flags::RF_CLASS_DEFAULT_OBJECT;
use ufathom_model::layout::{UFieldLayout, UStructLayout};

use crate::ctx::Ctx;
use crate::error::ScanError;
use crate::limits::{MAX_CHAIN_STEPS, MAX_REFLECTION_OFFSET, MAX_UOBJECT_OFFSET};
use crate::universe::{Anchors, Obj, Universe};
use crate::uobject::ObjectFields;

use ufathom_engine::NameTable;

/// First offset searched in a reflection class, past the `UObject` header.
const FIRST_REFLECTION_OFFSET: u32 = 0x18;

/// Most structs examined when looking for fields common to all of them.
const MAX_STRUCT_SAMPLES: usize = 3000;

/// The solved fields of `UField` and `UStruct`, and what depends on them.
#[derive(Debug, Clone)]
pub(crate) struct StructFields {
    pub ufield: UFieldLayout,
    /// Without `child_properties`, which the property step decides.
    pub ustruct: UStructLayout,
    pub uobject_flags: u32,
    pub uobject_size: u32,
    pub sizes: Sizes,
}

/// All `UClass` and `UScriptStruct` objects, up to a bound. The default
/// object of `Class` is an instance of `UClass` that is no class, and is left
/// out.
pub(crate) fn structs_of(universe: &Universe, table: &NameTable, anchors: &Anchors) -> Vec<Obj> {
    universe
        .objs
        .iter()
        .filter(|o| o.class == anchors.class || o.class == anchors.script_struct)
        .filter(|o| !universe.name(table, o).is_some_and(is_default_object_name))
        .take(MAX_STRUCT_SAMPLES)
        .copied()
        .collect()
}

fn offsets_8(from: u32, to: u32) -> impl Iterator<Item = u32> {
    (from.next_multiple_of(8)..to).step_by(8)
}

fn offsets_4(from: u32, to: u32) -> impl Iterator<Item = u32> {
    (from..to).step_by(4)
}

/// Whether `class` derives from `ancestor` through the super links.
pub(crate) fn derives_from(ctx: &Ctx<'_>, super_offset: u32, class: u64, ancestor: u64) -> bool {
    let mut current = class;
    for _ in 0..crate::limits::MAX_CLASS_HOPS * 8 {
        if current == ancestor {
            return true;
        }
        match ctx.u64(current + u64::from(super_offset)) {
            Some(next) if next != 0 => current = next,
            _ => return false,
        }
    }
    false
}

/// Whether a name is that of a class default object.
pub(crate) fn is_default_object_name(name: &str) -> bool {
    name.get(..9)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("Default__"))
        && name.len() > 9
}

/// Sizes of the reflection objects. A field of an object lies inside the
/// object, so an offset is only tried on objects large enough to hold it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Sizes {
    /// `sizeof(UClass)`.
    pub class: u32,
    /// `sizeof(UScriptStruct)`.
    pub script_struct: u32,
}

impl Sizes {
    /// The size of the object `owner`, by its class.
    pub(crate) fn of(&self, owner: &Obj, anchors: &Anchors) -> u32 {
        if owner.class == anchors.script_struct {
            self.script_struct
        } else {
            self.class
        }
    }
}

/// `SuperStruct`: the offset where every core class points at its parent.
fn solve_super(ctx: &Ctx<'_>, a: &Anchors) -> Result<u32, ScanError> {
    let relations = [
        (a.struct_, a.field),
        (a.class, a.struct_),
        (a.field, a.object),
        (a.function, a.struct_),
        (a.script_struct, a.struct_),
        (a.enum_, a.field),
        (a.package, a.object),
        (a.object, 0),
    ];
    let found: Vec<u32> = offsets_8(FIRST_REFLECTION_OFFSET, MAX_REFLECTION_OFFSET)
        .filter(|&offset| {
            relations
                .iter()
                .all(|&(child, parent)| ctx.u64(child + u64::from(offset)) == Some(parent))
        })
        .collect();
    let offset = ctx.resolve_among("ustruct.super_struct", &found)?;
    ctx.evidence(
        "ustruct.super_struct",
        u64::from(offset),
        "Struct.super == Field, Class.super == Struct, Field.super == Object, and five more core relations",
        relations.len(),
        relations.len(),
    );
    Ok(offset)
}

/// The size column of the anchors: `(address, expected sizes)`.
fn size_expectations(a: &Anchors) -> Vec<(u64, Vec<u32>)> {
    let mut out = Vec::new();
    let mut add = |name: &str, sizes: &[u32]| {
        if let Some(&addr) = a.structs.get(name) {
            out.push((addr, sizes.to_vec()));
        }
    };
    add("guid", &[0x10]);
    add("color", &[4]);
    add("vector", &[0xC, 0x18]);
    add("rotator", &[0xC, 0x18]);
    add("quat", &[0x10, 0x20]);
    out
}

fn solve_properties_size(ctx: &Ctx<'_>, a: &Anchors) -> Result<u32, ScanError> {
    let expectations = size_expectations(a);
    let found: Vec<u32> = offsets_4(FIRST_REFLECTION_OFFSET, MAX_REFLECTION_OFFSET)
        .filter(|&offset| {
            expectations.len() >= 2
                && expectations.iter().all(|(addr, sizes)| {
                    ctx.u32(addr + u64::from(offset))
                        .is_some_and(|v| sizes.contains(&v))
                })
        })
        .collect();
    let offset = ctx.resolve_among("ustruct.properties_size", &found)?;
    ctx.evidence(
        "ustruct.properties_size",
        u64::from(offset),
        "Guid is 0x10, Color is 4, Vector and Rotator are 0xC or 0x18",
        expectations.len(),
        expectations.len(),
    );
    Ok(offset)
}

/// Finds `MinAlignment` and the width of its storage.
fn solve_min_alignment(
    ctx: &Ctx<'_>,
    a: &Anchors,
    structs: &[Obj],
    properties_size: u32,
) -> Result<(u32, u32), ScanError> {
    let mut expectations: Vec<(u64, Vec<u32>)> = Vec::new();
    let mut add = |name: &str, values: &[u32]| {
        if let Some(&addr) = a.structs.get(name) {
            expectations.push((addr, values.to_vec()));
        }
    };
    add("guid", &[4]);
    add("color", &[4]);
    add("vector", &[4, 8]);
    add("rotator", &[4, 8]);
    add("transform", &[0x10]);
    let found: Vec<u32> = offsets_4(FIRST_REFLECTION_OFFSET, MAX_REFLECTION_OFFSET)
        .filter(|&offset| offset != properties_size)
        .filter(|&offset| {
            expectations.len() >= 3
                && expectations.iter().all(|(addr, values)| {
                    ctx.u32(addr + u64::from(offset))
                        .is_some_and(|v| values.contains(&(v & 0xFFFF)))
                })
        })
        .collect();
    let offset = ctx.resolve_among("ustruct.min_alignment", &found)?;
    ctx.evidence(
        "ustruct.min_alignment",
        u64::from(offset),
        "Guid and Color align to 4, Vector to 4 or 8, Transform to 16",
        expectations.len(),
        expectations.len(),
    );
    // Width: bytes above the low half belong to the next field when the value
    // is 16-bit. They are non-zero for some struct if that is so.
    let mut with_high_bits = 0usize;
    let mut examined = 0usize;
    for obj in structs {
        if let Some(value) = ctx.u32(obj.addr + u64::from(offset)) {
            examined += 1;
            if value >> 16 != 0 {
                with_high_bits += 1;
            }
        }
    }
    let (solved, method) = if with_high_bits > 0 {
        (
            2,
            "the two bytes after the alignment are non-zero in some structs",
        )
    } else {
        let hint = ctx.options.hint_number();
        let width = if hint.is_some_and(|h| h >= 506) { 2 } else { 4 };
        ctx.warn(format!(
            "ustruct.min_alignment_width: the two bytes after the alignment are zero in all \
             {examined} structs, so a 16-bit and a 32-bit field read the same; {width} follows the \
             engine era of the hint, not an observation"
        ));
        (
            width,
            "storage width is not observable; ordered by the hint",
        )
    };
    let width = ctx.resolve("ustruct.min_alignment_width", Some(solved), String::new)?;
    ctx.evidence(
        "ustruct.min_alignment_width",
        u64::from(width),
        method,
        examined,
        examined - with_high_bits,
    );
    Ok((offset, width))
}

/// `Children`: the pointer whose target's outer is the owner.
fn solve_children(
    ctx: &Ctx<'_>,
    universe: &Universe,
    structs: &[Obj],
    anchors: &Anchors,
    sizes: Sizes,
    super_offset: u32,
) -> Result<u32, ScanError> {
    let mut found: Vec<(u32, usize, usize)> = Vec::new();
    for offset in offsets_8(FIRST_REFLECTION_OFFSET, MAX_REFLECTION_OFFSET) {
        if offset == super_offset {
            continue;
        }
        let mut non_null = 0usize;
        let mut matched = 0usize;
        for owner in structs {
            if offset + 8 > sizes.of(owner, anchors) {
                continue;
            }
            let Some(value) = ctx.u64(owner.addr + u64::from(offset)) else {
                continue;
            };
            if value == 0 {
                continue;
            }
            non_null += 1;
            if universe
                .get(value)
                .is_some_and(|member| member.outer == owner.addr)
            {
                matched += 1;
            }
        }
        // Two owners suffice when every one of them agrees: a modern image
        // whose classes with functions are few still shows the pointer.
        let enough = non_null >= 3 && matched * 10 >= non_null * 9 || non_null == 2 && matched == 2;
        if enough {
            found.push((offset, matched, non_null));
        }
    }
    let offsets: Vec<u32> = found.iter().map(|&(o, _, _)| o).collect();
    let offset = ctx.resolve_among("ustruct.children", &offsets)?;
    let (matched, total) = found
        .iter()
        .find(|&&(o, _, _)| o == offset)
        .map_or((0, 0), |&(_, m, t)| (m, t));
    ctx.evidence(
        "ustruct.children",
        u64::from(offset),
        "pointer whose target's outer is the owning struct",
        total,
        matched,
    );
    Ok(offset)
}

/// Walks the member list from `head` through `next` while the outer stays
/// `owner`. Returns the number of members and whether the list ended at null.
fn walk_members(
    ctx: &Ctx<'_>,
    universe: &Universe,
    owner: u64,
    head: u64,
    next: u32,
) -> (usize, bool) {
    let mut seen = HashSet::new();
    let mut current = head;
    while current != 0 && seen.len() < MAX_CHAIN_STEPS {
        let belongs = universe.get(current).is_some_and(|o| o.outer == owner);
        if !belongs || !seen.insert(current) {
            return (seen.len(), false);
        }
        match ctx.u64(current + u64::from(next)) {
            Some(link) => current = link,
            None => return (seen.len(), false),
        }
    }
    (seen.len(), current == 0)
}

/// `UField::Next`: the offset that walks the longest member lists.
fn solve_next(
    ctx: &Ctx<'_>,
    universe: &Universe,
    structs: &[Obj],
    children: u32,
) -> Result<u32, ScanError> {
    let heads: Vec<(u64, u64)> = structs
        .iter()
        .filter_map(|owner| {
            let head = ctx.u64(owner.addr + u64::from(children))?;
            (head != 0).then_some((owner.addr, head))
        })
        .collect();
    let mut scored: Vec<(u32, usize, usize)> = Vec::new();
    for offset in offsets_8(8, MAX_UOBJECT_OFFSET + 0x40) {
        let mut members = 0usize;
        let mut ended = 0usize;
        for &(owner, head) in &heads {
            let (count, at_null) = walk_members(ctx, universe, owner, head, offset);
            members += count;
            ended += usize::from(at_null);
        }
        scored.push((offset, members, ended));
    }
    scored.sort_by_key(|&(offset, members, ended)| (std::cmp::Reverse((members, ended)), offset));
    let solved = match scored.as_slice() {
        [(offset, members, ended), rest @ ..]
            if *members > heads.len() * 2
                && *ended * 10 >= heads.len() * 9
                && rest.first().is_none_or(|r| (r.1, r.2) < (*members, *ended)) =>
        {
            Some(*offset)
        }
        _ => None,
    };
    let offset = ctx.resolve("ufield.next", solved, || {
        format!("no offset walks the member lists of {} owners", heads.len())
    })?;
    let (members, ended) = scored
        .iter()
        .find(|&&(o, _, _)| o == offset)
        .map_or((0, 0), |&(_, m, e)| (m, e));
    ctx.evidence(
        "ufield.next",
        u64::from(offset),
        format!(
            "walks {members} members of {} lists, {ended} ending at null, all sharing the owner as outer",
            heads.len()
        ),
        heads.len(),
        ended,
    );
    Ok(offset)
}

/// `ObjectFlags`: the 32-bit field whose bit 0x10 is set on every `Default__`
/// object and on almost no other.
pub(crate) fn solve_flags(
    ctx: &Ctx<'_>,
    universe: &Universe,
    table: &NameTable,
    fields: &ObjectFields,
    index_offset: u32,
) -> Result<u32, ScanError> {
    let defaults: Vec<&Obj> = universe
        .objs
        .iter()
        .filter(|o| universe.name(table, o).is_some_and(is_default_object_name))
        .take(4000)
        .collect();
    let others: Vec<&Obj> = {
        let step = (universe.objs.len() / 2000).max(1);
        universe
            .objs
            .iter()
            .step_by(step)
            .filter(|o| !universe.name(table, o).is_some_and(is_default_object_name))
            .collect()
    };
    let occupied: Vec<(u32, u32)> = {
        let name = fields.name;
        let fname = &fields.fname;
        let mut spans = vec![
            (index_offset, 4),
            (fields.class, 8),
            (fields.outer, 8),
            (name, fname.size),
        ];
        spans.push((0, 8));
        spans
    };
    let free = |offset: u32| {
        !occupied
            .iter()
            .any(|&(start, len)| offset < start + len && start < offset + 4)
    };
    let mut candidates: Vec<(u32, usize, usize)> = Vec::new();
    for offset in offsets_4(8, MAX_UOBJECT_OFFSET).filter(|&o| free(o)) {
        let set = |objs: &[&Obj]| -> usize {
            objs.iter()
                .filter(|o| {
                    ctx.u32(o.addr + u64::from(offset))
                        .is_some_and(|v| v & RF_CLASS_DEFAULT_OBJECT != 0)
                })
                .count()
        };
        let with_default = set(&defaults);
        let with_other = set(&others);
        if defaults.len() >= 4
            && with_default * 100 >= defaults.len() * 99
            && with_other * 100 <= others.len()
        {
            candidates.push((offset, with_default, defaults.len() + others.len()));
        }
    }
    let offsets: Vec<u32> = candidates.iter().map(|&(o, _, _)| o).collect();
    let offset = ctx.resolve_among("uobject.flags", &offsets)?;
    ctx.evidence(
        "uobject.flags",
        u64::from(offset),
        format!(
            "RF_ClassDefaultObject (0x10) set on {} Default__ objects, on at most 1% of {} others",
            defaults.len(),
            others.len()
        ),
        defaults.len() + others.len(),
        defaults.len(),
    );
    Ok(offset)
}

/// Solves `UField`, `UStruct` and the flags and size of `UObject`.
pub(crate) fn solve_structs(
    ctx: &Ctx<'_>,
    universe: &Universe,
    table: &NameTable,
    fields: &ObjectFields,
    anchors: &Anchors,
    index_offset: u32,
) -> Result<StructFields, ScanError> {
    let structs = structs_of(universe, table, anchors);
    let super_struct = solve_super(ctx, anchors)?;
    let properties_size = solve_properties_size(ctx, anchors)?;
    let (min_alignment, min_alignment_width) =
        solve_min_alignment(ctx, anchors, &structs, properties_size)?;
    let sizes = Sizes {
        class: ctx
            .u32(anchors.class + u64::from(properties_size))
            .unwrap_or(0),
        script_struct: ctx
            .u32(anchors.script_struct + u64::from(properties_size))
            .unwrap_or(0),
    };
    let children = solve_children(ctx, universe, &structs, anchors, sizes, super_struct)?;
    let next = solve_next(ctx, universe, &structs, children)?;
    let uobject_flags = solve_flags(ctx, universe, table, fields, index_offset)?;
    let size_solved = ctx.u32(anchors.object + u64::from(properties_size));
    let uobject_size = ctx.resolve("uobject.size", size_solved, || {
        "the Object class has no readable PropertiesSize".to_owned()
    })?;
    ctx.evidence(
        "uobject.size",
        u64::from(uobject_size),
        "PropertiesSize of the Object class",
        1,
        1,
    );
    if next < uobject_size {
        return Err(ScanError::ValidationFailed {
            invariant: "UField::Next lies past the end of UObject",
            checked: 1,
            violations: 1,
            example: format!("next {next:#x}, sizeof(UObject) {uobject_size:#x}"),
        });
    }
    Ok(StructFields {
        ufield: UFieldLayout { next },
        ustruct: UStructLayout {
            super_struct,
            children,
            child_properties: None,
            properties_size,
            min_alignment,
            min_alignment_width,
        },
        uobject_flags,
        uobject_size,
        sizes,
    })
}
