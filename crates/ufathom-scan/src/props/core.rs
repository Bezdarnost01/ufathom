//! The members every property has: element size, array dimension, flags and
//! the member offset.
//!
//! - `ElementSize`: the 32-bit field that reads the C++ size of every
//!   primitive property (`IntProperty` 4, `DoubleProperty` 8, ...).
//! - `ArrayDim`: reads 1 in nearly every member and sits next to the element
//!   size.
//! - `Offset_Internal`: for structs made of one primitive repeated (`Guid`,
//!   `Color`, `Vector`), the members sit at `0, s, 2s, ...`.
//! - `PropertyFlags`: the 64-bit field where function parameters have
//!   `CPF_Parm`, `ReturnValue` also has `CPF_ReturnParm`, and members have
//!   neither.

use std::collections::BTreeMap;

use ufathom_model::flags::{CPF_PARM, CPF_RETURN_PARM};

use crate::agree::agrees;
use crate::error::ScanError;
use crate::limits::{FIELD_AGREEMENT_PERCENT, MAX_PROPERTY_OFFSET};

use super::{Env, Prop, Role};

/// The solved members of the property base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Core {
    pub array_dim: u32,
    pub element_size: u32,
    pub flags: u32,
    pub offset_internal: u32,
}

/// The C++ size of the value a primitive property class stores.
fn intrinsic_size(class_name: &str) -> Option<u32> {
    Some(match class_name {
        "Int8Property" | "ByteProperty" => 1,
        "Int16Property" | "UInt16Property" => 2,
        "IntProperty" | "UInt32Property" | "FloatProperty" => 4,
        "Int64Property" | "UInt64Property" | "DoubleProperty" => 8,
        _ => return None,
    })
}

fn share(matched: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        matched as f64 / total as f64
    }
}

/// Candidates this far above the lowest one are dropped: the members of a
/// property lie within one small object, and a field that matches only far
/// beyond it is read from the neighbouring allocation.
const NEIGHBOURHOOD: u32 = 0x40;

/// Narrows candidates to those within [`NEIGHBOURHOOD`] of the lowest.
fn near_lowest(mut candidates: Vec<u32>) -> Vec<u32> {
    candidates.sort_unstable();
    match candidates.first().copied() {
        Some(lowest) => candidates
            .into_iter()
            .filter(|&c| c - lowest < NEIGHBOURHOOD)
            .collect(),
        None => candidates,
    }
}

fn candidates_4() -> impl Iterator<Item = u32> {
    (8..MAX_PROPERTY_OFFSET).step_by(4)
}

fn solve_element_size(env: &Env<'_, '_>, props: &[Prop]) -> Result<u32, ScanError> {
    let ctx = env.ctx;
    let primitives: Vec<(&Prop, u32)> = props
        .iter()
        .filter(|p| p.role == Role::Member)
        .filter_map(|p| Some((p, intrinsic_size(&p.class_name)?)))
        .collect();
    let mut found = Vec::new();
    for offset in candidates_4() {
        let matched = primitives
            .iter()
            .filter(|&&(p, size)| ctx.u32(p.addr + u64::from(offset)) == Some(size))
            .count();
        if primitives.len() >= 8 && agrees(matched, primitives.len(), FIELD_AGREEMENT_PERCENT) {
            found.push((offset, matched));
        }
    }
    let offsets = near_lowest(found.iter().map(|&(o, _)| o).collect());
    let offset = ctx.resolve_among("properties.element_size", &offsets)?;
    let matched = found
        .iter()
        .find(|&&(o, _)| o == offset)
        .map_or(0, |&(_, m)| m);
    ctx.evidence(
        "properties.element_size",
        u64::from(offset),
        "reads the C++ size of every primitive property (Guid members read 4, Color members 1)",
        primitives.len(),
        matched,
    );
    Ok(offset)
}

fn solve_array_dim(env: &Env<'_, '_>, props: &[Prop], element_size: u32) -> Result<u32, ScanError> {
    let ctx = env.ctx;
    let members: Vec<&Prop> = props.iter().filter(|p| p.role == Role::Member).collect();
    let mut found = Vec::new();
    for offset in candidates_4().filter(|&o| o != element_size) {
        let ones = members
            .iter()
            .filter(|p| ctx.u32(p.addr + u64::from(offset)) == Some(1))
            .count();
        if members.len() >= 8 && agrees(ones, members.len(), FIELD_AGREEMENT_PERCENT) {
            found.push((offset, ones));
        }
    }
    // The dimension sits just before the element size in every layout seen;
    // other fields that read 1 are decoys.
    let adjacent: Vec<u32> = found
        .iter()
        .map(|&(o, _)| o)
        .filter(|&o| o + 4 == element_size)
        .collect();
    let offsets: Vec<u32> = if adjacent.is_empty() {
        near_lowest(found.iter().map(|&(o, _)| o).collect())
    } else {
        adjacent
    };
    let offset = ctx.resolve_among("properties.array_dim", &offsets)?;
    let ones = found
        .iter()
        .find(|&&(o, _)| o == offset)
        .map_or(0, |&(_, m)| m);
    ctx.evidence(
        "properties.array_dim",
        u64::from(offset),
        "reads 1 in nearly every member and sits next to the element size",
        members.len(),
        ones,
    );
    Ok(offset)
}

fn solve_offset_internal(
    env: &Env<'_, '_>,
    props: &[Prop],
    element_size: u32,
    array_dim: u32,
) -> Result<u32, ScanError> {
    let ctx = env.ctx;
    let mut by_owner: BTreeMap<u64, Vec<&Prop>> = BTreeMap::new();
    for prop in props.iter().filter(|p| p.role == Role::Member) {
        by_owner.entry(prop.owner).or_default().push(prop);
    }
    // Structs of one primitive repeated, without padding: member k is at k*s.
    let packed: Vec<(Vec<&Prop>, u32)> = by_owner
        .into_values()
        .filter(|members| members.len() >= 3 && members.len() <= 8)
        .filter_map(|members| {
            let first = members.first()?;
            let size = intrinsic_size(&first.class_name)?;
            let same = members.iter().all(|m| {
                m.class_name == first.class_name
                    && ctx.u32(m.addr + u64::from(array_dim)) == Some(1)
            });
            same.then_some((members, size))
        })
        .collect();
    tracing::debug!(
        packed = packed.len(),
        "packed structs for the member offset"
    );
    let mut found = Vec::new();
    for offset in candidates_4().filter(|&o| o != element_size && o != array_dim) {
        let agreeing = packed
            .iter()
            .filter(|(members, size)| {
                let mut got: Vec<u32> = members
                    .iter()
                    .filter_map(|m| ctx.u32(m.addr + u64::from(offset)))
                    .collect();
                got.sort_unstable();
                let want: Vec<u32> = (0..members.len() as u32).map(|k| k * size).collect();
                got == want
            })
            .count();
        tracing::trace!(offset, agreeing, "member offset candidate");
        if packed.len() >= 3 && agreeing >= 3 && share(agreeing, packed.len()) >= 0.6 {
            found.push((offset, agreeing));
        }
    }
    let offsets = near_lowest(found.iter().map(|&(o, _)| o).collect());
    let offset = ctx.resolve_among("properties.offset_internal", &offsets)?;
    let agreeing = found
        .iter()
        .find(|&&(o, _)| o == offset)
        .map_or(0, |&(_, m)| m);
    ctx.evidence(
        "properties.offset_internal",
        u64::from(offset),
        "Guid members read 0, 4, 8, 12 and Color members 0, 1, 2, 3 in some order",
        packed.len(),
        agreeing,
    );
    Ok(offset)
}

fn solve_flags(env: &Env<'_, '_>, props: &[Prop]) -> Result<u32, ScanError> {
    let ctx = env.ctx;
    let params: Vec<&Prop> = props.iter().filter(|p| p.role == Role::Param).collect();
    let members: Vec<&Prop> = props.iter().filter(|p| p.role == Role::Member).collect();
    let returns: Vec<&Prop> = params
        .iter()
        .copied()
        .filter(|p| p.name == "ReturnValue")
        .collect();
    let mut found = Vec::new();
    for offset in (8..MAX_PROPERTY_OFFSET).step_by(8) {
        let flags = |p: &&Prop| ctx.u64(p.addr + u64::from(offset));
        let params_ok = params
            .iter()
            .filter(|p| flags(p).is_some_and(|v| v & CPF_PARM != 0))
            .count();
        let members_ok = members
            .iter()
            .filter(|p| flags(p).is_some_and(|v| v & (CPF_PARM | CPF_RETURN_PARM) == 0))
            .count();
        let returns_ok = returns
            .iter()
            .filter(|p| {
                flags(p)
                    .is_some_and(|v| v & (CPF_PARM | CPF_RETURN_PARM) == CPF_PARM | CPF_RETURN_PARM)
            })
            .count();
        let enough = !returns.is_empty() && params.len() >= 4 && members.len() >= 8;
        if enough
            && agrees(params_ok, params.len(), FIELD_AGREEMENT_PERCENT)
            && agrees(members_ok, members.len(), FIELD_AGREEMENT_PERCENT)
            && returns_ok == returns.len()
        {
            found.push((offset, params_ok + members_ok));
        }
    }
    let offsets = near_lowest(found.iter().map(|&(o, _)| o).collect());
    let offset = ctx.resolve_among("properties.flags", &offsets)?;
    ctx.evidence(
        "properties.flags",
        u64::from(offset),
        "ReturnValue has Parm and ReturnParm, other parameters Parm, members neither",
        params.len() + members.len(),
        found
            .iter()
            .find(|&&(o, _)| o == offset)
            .map_or(0, |&(_, m)| m),
    );
    Ok(offset)
}

/// Solves the core members from the sampled properties.
pub(crate) fn solve_core(
    env: &Env<'_, '_>,
    _system: &super::System,
    props: &[Prop],
) -> Result<Core, ScanError> {
    let element_size = solve_element_size(env, props)?;
    let array_dim = solve_array_dim(env, props, element_size)?;
    let offset_internal = solve_offset_internal(env, props, element_size, array_dim)?;
    let flags = solve_flags(env, props)?;
    Ok(Core {
        array_dim,
        element_size,
        flags,
        offset_internal,
    })
}
