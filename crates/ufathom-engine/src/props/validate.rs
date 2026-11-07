//! Checking decoded members against their owner.

use super::{PropertyKind, RawProperty};
use crate::error::EngineError;

/// Checks the members of one struct against its size and each other.
///
/// Reports, one error per problem and never stopping at the first:
///
/// - an element size or array dimension of zero,
/// - a member that ends past `owner_size` (`offset + element_size * array_dim`),
/// - two members whose byte ranges overlap. Bit-field booleans that share an
///   offset do not count as overlapping.
///
/// Decoding never derives a layout from a bad size, so this is how a caller
/// learns that a member cannot be trusted. All arithmetic is checked.
#[must_use]
pub fn validate_members(owner_size: u32, members: &[RawProperty]) -> Vec<EngineError> {
    let mut problems = Vec::new();
    let mut ranges: Vec<(u64, u64, &RawProperty)> = Vec::with_capacity(members.len());
    for member in members {
        let bad = |reason: String| EngineError::Inconsistent {
            addr: member.address,
            what: "property",
            reason: format!("{}: {reason}", member.name),
        };
        if member.element_size == 0 || member.array_dim == 0 {
            problems.push(bad(format!(
                "element size {} and dimension {}",
                member.element_size, member.array_dim
            )));
        }
        let size = u64::from(member.element_size).saturating_mul(u64::from(member.array_dim));
        let start = u64::from(member.offset);
        let end = start.saturating_add(size);
        if end > u64::from(owner_size) {
            problems.push(bad(format!(
                "bytes {start:#x} to {end:#x} do not fit the owner size {owner_size:#x}"
            )));
        }
        ranges.push((start, end, member));
    }
    ranges.sort_by_key(|&(start, end, _)| (start, end));
    let mut widest: Option<(u64, &RawProperty)> = None;
    for &(start, end, member) in &ranges {
        if let Some((reach, previous)) = widest {
            let shares_bits = previous.offset == member.offset
                && PropertyKind::from_class_name(&previous.class_name) == PropertyKind::Bool
                && PropertyKind::from_class_name(&member.class_name) == PropertyKind::Bool;
            if start < reach && !shares_bits {
                problems.push(EngineError::Inconsistent {
                    addr: member.address,
                    what: "property",
                    reason: format!(
                        "{} at {start:#x} overlaps {} which reaches {reach:#x}",
                        member.name, previous.name
                    ),
                });
            }
        }
        if widest.is_none_or(|(reach, _)| end > reach) {
            widest = Some((end, member));
        }
    }
    problems
}
