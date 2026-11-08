//! The layout planner: places resolved fields at their exact offsets.
//!
//! The planner works on already-resolved types (a Rust type with a size and an
//! alignment) and never looks at the dump. Its one promise: the plan it
//! returns, written as a `#[repr(C, align(N))]` struct with explicit padding,
//! has exactly the size and field offsets that the input states, or the
//! offending field is degraded to bytes or left out. Nothing is ever placed
//! "close enough".

use crate::report::DegradeReason;

/// Largest alignment accepted for a declared type alignment.
const MAX_ALIGN: u32 = 4096;

/// The parent type, as far as the planner needs to know.
#[derive(Debug, Clone)]
pub(crate) struct BaseInfo {
    /// Size of the Rust type of the parent. Equal to its size in the dump.
    pub size: u32,
    /// Alignment of the Rust type of the parent.
    pub align: u32,
}

/// A field with its type already resolved.
#[derive(Debug, Clone)]
pub(crate) struct PlanField {
    /// Offset from the start of the owner.
    pub offset: u32,
    /// What the field is.
    pub kind: FieldKind,
}

/// The shape of one field.
#[derive(Debug, Clone)]
pub(crate) enum FieldKind {
    /// A Rust type for each of `count` elements of `size` bytes.
    Typed {
        /// Size of one element in bytes.
        size: u32,
        /// Alignment of the Rust type.
        align: u32,
        /// Number of elements (the C array dimension).
        count: u32,
    },
    /// A bit of a bit-field `bool`.
    Bit {
        /// Byte of the storage unit that holds the bit.
        byte_offset: u8,
        /// Mask of the bit in that byte.
        mask: u8,
        /// Size in bytes of the storage unit.
        field_size: u8,
    },
    /// Cannot be a Rust type: `span` raw bytes, with the reason.
    Opaque {
        /// Number of bytes.
        span: u32,
        /// Why it is opaque.
        reason: DegradeReason,
    },
}

/// What the planner is asked to place.
#[derive(Debug, Clone)]
pub(crate) struct PlanInput {
    /// Exact size of the owner in bytes.
    pub size: u32,
    /// Alignment the dump declares. Anything that is not a power of two is
    /// treated as 1. This is evidence, not a promise: the planner raises it
    /// from the parent and from the members (see [`plan`]).
    pub align_hint: u32,
    /// The parent, if there is one.
    pub base: Option<BaseInfo>,
    /// The fields. Any order: the planner sorts them by offset.
    pub fields: Vec<PlanField>,
}

/// How the parent is represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BaseMode {
    /// No parent.
    None,
    /// A `base: Parent` field at offset 0.
    Embedded,
    /// The parent is not a field. Its bytes are the leading padding and the
    /// parent can be viewed by a pointer cast, because it fits and is not more
    /// aligned than the owner.
    Cast(DegradeReason, String),
    /// The parent is not a field and cannot be viewed either.
    Unavailable(DegradeReason, String),
}

/// A bit of a storage group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PlacedBit {
    /// Index of the field in the input.
    pub field: usize,
    /// Index inside the storage bytes of the group.
    pub byte: u32,
    /// Mask in that byte.
    pub mask: u8,
}

/// One thing in memory order, after the base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    /// Explicit padding.
    Pad {
        /// Offset.
        offset: u32,
        /// Length in bytes.
        len: u32,
    },
    /// A field with its Rust type.
    Typed {
        /// Index in the input.
        field: usize,
        /// Offset.
        offset: u32,
        /// Number of elements.
        count: u32,
    },
    /// A field degraded to raw bytes.
    Bytes {
        /// Index in the input.
        field: usize,
        /// Offset.
        offset: u32,
        /// Length in bytes.
        len: u32,
    },
    /// Raw storage shared by bit-field `bool`s.
    Bits {
        /// Offset of the storage.
        offset: u32,
        /// Length of the storage in bytes.
        len: u32,
        /// The bits it holds.
        bits: Vec<PlacedBit>,
    },
}

/// A field that has no storage of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Omitted {
    /// Index in the input.
    pub field: usize,
    /// Why.
    pub reason: DegradeReason,
}

/// A recorded degradation: field index (or `None` for the base).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Note {
    /// The field, or `None` for the base class.
    pub field: Option<usize>,
    /// Why.
    pub reason: DegradeReason,
    /// Explanation.
    pub detail: String,
}

/// The result of planning one type.
#[derive(Debug, Clone)]
pub(crate) struct Plan {
    /// The alignment of the Rust type: what `align(N)` is declared as.
    pub align: u32,
    /// What the alignment would be from the declared hint alone, after the
    /// size clamp. `align` is above it when the planner raised it.
    pub declared_align: u32,
    /// How the parent is represented.
    pub base: BaseMode,
    /// What follows the base, in memory order, with explicit padding.
    pub items: Vec<Item>,
    /// Fields with no storage.
    pub omitted: Vec<Omitted>,
    /// Everything that was degraded or omitted.
    pub notes: Vec<Note>,
}

fn is_pow2(value: u32) -> bool {
    value.is_power_of_two() && value <= MAX_ALIGN
}

/// The alignment the Rust type gets: the declared one, lowered while it does
/// not divide the size. (A size that is not a multiple of the alignment cannot
/// be a Rust type size.)
fn effective_align(size: u32, hint: u32) -> u32 {
    let mut align = if is_pow2(hint) { hint } else { 1 };
    while align > 1 && size % align != 0 {
        align /= 2;
    }
    align
}

/// The alignment the owner must have at least, as far as the evidence shows.
///
/// Some engine builds record no alignment for most native classes, and blueprint
/// classes can record less than their pointer members need. The native compiler
/// had to align every member and the parent anyway, so the hint is the largest
/// of:
///
/// - the declared alignment,
/// - the alignment of the parent as planned (the caller passes the Rust
///   alignment of the generated parent, not the raw dump value),
/// - the alignment of every typed field that sits inside the owner at an offset
///   that is a multiple of its alignment.
///
/// A field at a misaligned offset is no evidence of anything: it degrades to
/// bytes instead of raising the alignment. Raising never changes the size or
/// an offset; [`effective_align`] still clamps the result to the size.
fn alignment_hint(input: &PlanInput) -> u32 {
    let mut hint = if is_pow2(input.align_hint) {
        input.align_hint
    } else {
        1
    };
    if let Some(base) = &input.base {
        if is_pow2(base.align) {
            hint = hint.max(base.align);
        }
    }
    for field in &input.fields {
        let FieldKind::Typed { size, align, count } = field.kind else {
            continue;
        };
        let end = u64::from(field.offset) + u64::from(size) * u64::from(count);
        if is_pow2(align) && field.offset % align == 0 && end <= u64::from(input.size) {
            hint = hint.max(align);
        }
    }
    hint
}

enum Slot {
    Field(usize),
    Group(Vec<usize>),
}

struct Placer {
    size: u32,
    cursor: u32,
    items: Vec<Item>,
}

impl Placer {
    /// Reserves `[start, start + len)`, padding the gap before it.
    fn reserve(&mut self, start: u32, len: u32) -> Result<(), DegradeReason> {
        if start < self.cursor {
            return Err(DegradeReason::Overlap);
        }
        let end = u64::from(start) + u64::from(len);
        if end > u64::from(self.size) {
            return Err(DegradeReason::OutOfBounds);
        }
        if start > self.cursor {
            self.items.push(Item::Pad {
                offset: self.cursor,
                len: start - self.cursor,
            });
        }
        self.cursor = start + len;
        Ok(())
    }
}

/// Places the fields of one type. See the module documentation.
///
/// The alignment of the result is the declared one raised as described at
/// [`alignment_hint`], then lowered while it does not divide the size.
pub(crate) fn plan(input: &PlanInput) -> Plan {
    let size = input.size;
    let declared_align = effective_align(size, input.align_hint);
    let align = effective_align(size, alignment_hint(input));
    let mut notes: Vec<Note> = Vec::new();
    let mut omitted: Vec<Omitted> = Vec::new();

    // Slots in offset order. Bit-field storage that overlaps is one group.
    let mut order: Vec<usize> = (0..input.fields.len()).collect();
    order.sort_by_key(|&i| input.fields.get(i).map_or(0, |f| f.offset));
    let mut slots: Vec<(u32, Slot)> = Vec::new();
    // (start, end, members) of the group being built.
    let mut open: Option<(u32, u32, Vec<usize>)> = None;
    let storage = |f: &PlanField| match f.kind {
        FieldKind::Bit {
            byte_offset,
            field_size,
            ..
        } => Some((
            f.offset,
            f.offset
                .saturating_add(u32::from(field_size.max(byte_offset.saturating_add(1)))),
        )),
        _ => None,
    };
    let flush = |open: &mut Option<(u32, u32, Vec<usize>)>, slots: &mut Vec<(u32, Slot)>| {
        if let Some((start, _end, members)) = open.take() {
            slots.push((start, Slot::Group(members)));
        }
    };
    for &i in &order {
        let Some(field) = input.fields.get(i) else {
            continue;
        };
        match storage(field) {
            Some((start, end)) => {
                if let Some((_, group_end, members)) = open.as_mut() {
                    if start < *group_end {
                        *group_end = (*group_end).max(end);
                        members.push(i);
                        continue;
                    }
                }
                flush(&mut open, &mut slots);
                open = Some((start, end, vec![i]));
            }
            None => {
                flush(&mut open, &mut slots);
                slots.push((field.offset, Slot::Field(i)));
            }
        }
    }
    flush(&mut open, &mut slots);
    // Stable: equal starts keep their input order.
    slots.sort_by_key(|(start, _)| *start);

    // The parent.
    let first_offset = slots.first().map_or(size, |(start, _)| *start);
    let (base, cursor) = match &input.base {
        None => (BaseMode::None, 0),
        Some(b) if b.size > size => (
            BaseMode::Unavailable(
                DegradeReason::BaseNotEmbedded,
                format!(
                    "parent is {:#x} bytes, larger than the type ({size:#x})",
                    b.size
                ),
            ),
            0,
        ),
        Some(b) if b.align > align => (
            BaseMode::Unavailable(
                DegradeReason::BaseNotEmbedded,
                format!(
                    "parent alignment {} is above the type alignment {align}",
                    b.align
                ),
            ),
            0,
        ),
        Some(b) if b.size > first_offset => (
            BaseMode::Cast(
                DegradeReason::BaseNotEmbedded,
                format!(
                    "parent is {:#x} bytes but the first field is at {first_offset:#x}",
                    b.size
                ),
            ),
            0,
        ),
        Some(b) => (BaseMode::Embedded, b.size),
    };
    match &base {
        BaseMode::Cast(reason, detail) | BaseMode::Unavailable(reason, detail) => {
            notes.push(Note {
                field: None,
                reason: *reason,
                detail: detail.clone(),
            });
        }
        BaseMode::None | BaseMode::Embedded => {}
    }
    let mut placer = Placer {
        size,
        cursor,
        items: Vec::new(),
    };

    for (_, slot) in &slots {
        match slot {
            Slot::Field(i) => {
                let Some(field) = input.fields.get(*i) else {
                    continue;
                };
                place_field(&mut placer, *i, field, align, &mut omitted, &mut notes);
            }
            Slot::Group(members) => {
                place_group(&mut placer, input, members, &mut omitted, &mut notes);
            }
        }
    }
    if placer.cursor < size {
        let offset = placer.cursor;
        placer.items.push(Item::Pad {
            offset,
            len: size - offset,
        });
    }

    Plan {
        align,
        declared_align,
        base,
        items: placer.items,
        omitted,
        notes,
    }
}

fn omit(
    index: usize,
    reason: DegradeReason,
    detail: String,
    omitted: &mut Vec<Omitted>,
    notes: &mut Vec<Note>,
) {
    omitted.push(Omitted {
        field: index,
        reason,
    });
    notes.push(Note {
        field: Some(index),
        reason,
        detail,
    });
}

fn place_field(
    placer: &mut Placer,
    index: usize,
    field: &PlanField,
    align: u32,
    omitted: &mut Vec<Omitted>,
    notes: &mut Vec<Note>,
) {
    let offset = field.offset;
    // The byte span, and for a typed field what makes it unrepresentable.
    let (span, problem) = match field.kind {
        FieldKind::Typed {
            size,
            align: field_align,
            count,
        } => {
            let span = u64::from(size) * u64::from(count);
            // Misalignment first: such a field says nothing about the owner's
            // alignment, so "the owner is too low" would be the wrong reason.
            let problem = if field_align > 1 && offset % field_align != 0 {
                Some((
                    DegradeReason::Misaligned,
                    format!("offset {offset:#x} is not a multiple of {field_align}"),
                ))
            } else if field_align > align {
                Some((
                    DegradeReason::AlignmentTooLow,
                    format!("type needs alignment {field_align}, the owner has {align}"),
                ))
            } else {
                None
            };
            (span, problem)
        }
        FieldKind::Opaque { span, reason } => (
            u64::from(span),
            Some((reason, "no exact Rust type".to_owned())),
        ),
        // Handled in groups.
        FieldKind::Bit { .. } => return,
    };
    let Ok(len) = u32::try_from(span) else {
        omit(
            index,
            DegradeReason::OutOfBounds,
            format!("span {span:#x} does not fit in 32 bits"),
            omitted,
            notes,
        );
        return;
    };
    if let Err(reason) = placer.reserve(offset, len) {
        let detail = match reason {
            DegradeReason::Overlap => format!(
                "starts at {offset:#x}, before the end of the previous field ({:#x})",
                placer.cursor
            ),
            _ => format!(
                "{offset:#x} + {len:#x} is past the end of the type ({:#x})",
                placer.size
            ),
        };
        omit(index, reason, detail, omitted, notes);
        return;
    }
    match (&field.kind, problem) {
        (FieldKind::Typed { count, .. }, None) => placer.items.push(Item::Typed {
            field: index,
            offset,
            count: *count,
        }),
        (_, problem) => {
            if let Some((reason, detail)) = problem {
                // An opaque field was already reported by the resolver.
                if !matches!(field.kind, FieldKind::Opaque { .. }) {
                    notes.push(Note {
                        field: Some(index),
                        reason,
                        detail,
                    });
                }
            }
            placer.items.push(Item::Bytes {
                field: index,
                offset,
                len,
            });
        }
    }
}

fn place_group(
    placer: &mut Placer,
    input: &PlanInput,
    members: &[usize],
    omitted: &mut Vec<Omitted>,
    notes: &mut Vec<Note>,
) {
    // Validate each bit; an unaddressable one has no accessor.
    let mut valid: Vec<(usize, u32, u8, u32)> = Vec::new(); // field, absolute byte, mask, unit end
    for &i in members {
        let Some(field) = input.fields.get(i) else {
            continue;
        };
        let FieldKind::Bit {
            byte_offset,
            mask,
            field_size,
        } = field.kind
        else {
            continue;
        };
        if mask == 0 || byte_offset >= field_size {
            omit(
                i,
                DegradeReason::InvalidBitfield,
                format!("bit in byte {byte_offset} of a {field_size}-byte unit, mask {mask:#04x}"),
                omitted,
                notes,
            );
            continue;
        }
        valid.push((
            i,
            field.offset + u32::from(byte_offset),
            mask,
            field.offset + u32::from(field_size),
        ));
    }
    let Some(start) = valid
        .iter()
        .map(|&(i, ..)| input.fields.get(i).map_or(u32::MAX, |f| f.offset))
        .min()
    else {
        return;
    };
    let end = valid
        .iter()
        .map(|&(_, _, _, end)| end)
        .max()
        .unwrap_or(start);
    let len = end - start;
    if let Err(reason) = placer.reserve(start, len) {
        for &(i, abs, ..) in &valid {
            omit(
                i,
                reason,
                format!("bit storage {start:#x}..{end:#x} (bit at {abs:#x}) does not fit"),
                omitted,
                notes,
            );
        }
        return;
    }
    let bits = valid
        .iter()
        .map(|&(field, abs, mask, _)| PlacedBit {
            field,
            byte: abs - start,
            mask,
        })
        .collect();
    placer.items.push(Item::Bits {
        offset: start,
        len,
        bits,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(offset: u32, size: u32, align: u32) -> PlanField {
        PlanField {
            offset,
            kind: FieldKind::Typed {
                size,
                align,
                count: 1,
            },
        }
    }

    fn bit(offset: u32, byte_offset: u8, mask: u8) -> PlanField {
        PlanField {
            offset,
            kind: FieldKind::Bit {
                byte_offset,
                mask,
                field_size: 1,
            },
        }
    }

    fn input(size: u32, align: u32, fields: Vec<PlanField>) -> PlanInput {
        PlanInput {
            size,
            align_hint: align,
            base: None,
            fields,
        }
    }

    fn reasons(plan: &Plan) -> Vec<DegradeReason> {
        plan.notes.iter().map(|n| n.reason).collect()
    }

    #[test]
    fn plain_fields_get_explicit_padding() {
        let p = plan(&input(24, 8, vec![typed(0, 4, 4), typed(8, 8, 8)]));
        assert_eq!(p.align, 8);
        assert_eq!(
            p.items,
            vec![
                Item::Typed {
                    field: 0,
                    offset: 0,
                    count: 1
                },
                Item::Pad { offset: 4, len: 4 },
                Item::Typed {
                    field: 1,
                    offset: 8,
                    count: 1
                },
                Item::Pad { offset: 16, len: 8 },
            ]
        );
        assert!(p.notes.is_empty());
    }

    #[test]
    fn overlapping_field_has_no_storage() {
        let p = plan(&input(16, 8, vec![typed(0, 8, 8), typed(4, 4, 4)]));
        assert_eq!(reasons(&p), vec![DegradeReason::Overlap]);
        assert_eq!(p.omitted.len(), 1);
        assert!(
            !p.items
                .iter()
                .any(|i| matches!(i, Item::Typed { field: 1, .. }))
        );
    }

    #[test]
    fn misaligned_field_becomes_bytes() {
        let p = plan(&input(16, 8, vec![typed(2, 4, 4)]));
        assert_eq!(reasons(&p), vec![DegradeReason::Misaligned]);
        assert!(p.items.contains(&Item::Bytes {
            field: 0,
            offset: 2,
            len: 4
        }));
    }

    #[test]
    fn declared_alignment_of_one_is_raised_by_pointer_fields() {
        // A class that records alignment 1 but holds aligned pointers.
        let p = plan(&input(24, 1, vec![typed(8, 8, 8), typed(16, 8, 8)]));
        assert_eq!(p.declared_align, 1);
        assert_eq!(p.align, 8);
        assert!(p.notes.is_empty(), "{:?}", p.notes);
        assert!(
            p.items
                .iter()
                .filter(|i| matches!(i, Item::Typed { .. }))
                .count()
                == 2
        );
    }

    #[test]
    fn the_largest_aligned_field_decides() {
        let p = plan(&input(
            0x50,
            1,
            vec![typed(0x10, 8, 8), typed(0x20, 16, 16)],
        ));
        assert_eq!(p.align, 16);
        assert!(p.notes.is_empty());
    }

    #[test]
    fn parent_alignment_raises_the_child() {
        let mut i = input(0x28, 1, vec![typed(0x20, 4, 4)]);
        i.base = Some(BaseInfo {
            size: 0x20,
            align: 16,
        });
        let p = plan(&i);
        // 0x28 is a multiple of 8, not 16: the clamp lowers the raised hint.
        assert_eq!(p.align, 8);
        assert!(matches!(p.base, BaseMode::Unavailable(..)));

        let mut j = input(0x30, 1, vec![typed(0x20, 4, 4)]);
        j.base = Some(BaseInfo {
            size: 0x20,
            align: 16,
        });
        let p = plan(&j);
        assert_eq!(p.align, 16);
        assert_eq!(p.declared_align, 1);
        assert_eq!(p.base, BaseMode::Embedded);
    }

    #[test]
    fn misaligned_field_does_not_raise_the_alignment() {
        let p = plan(&input(16, 1, vec![typed(2, 4, 4), typed(8, 2, 2)]));
        // The 4-byte field at offset 2 is no evidence. The 2-byte one is.
        assert_eq!(p.align, 2);
        assert_eq!(reasons(&p), vec![DegradeReason::Misaligned]);
        assert!(p.items.contains(&Item::Bytes {
            field: 0,
            offset: 2,
            len: 4
        }));
        let only = plan(&input(16, 1, vec![typed(4, 8, 8)]));
        assert_eq!(only.align, 1);
        assert_eq!(reasons(&only), vec![DegradeReason::Misaligned]);
    }

    #[test]
    fn field_outside_the_type_does_not_raise_the_alignment() {
        let outside = plan(&input(16, 1, vec![typed(16, 8, 8)]));
        assert_eq!(outside.align, 1);
        let overhang = plan(&input(16, 1, vec![typed(0, 32, 16)]));
        assert_eq!(overhang.align, 1);
        assert!(reasons(&overhang).contains(&DegradeReason::OutOfBounds));
    }

    #[test]
    fn opaque_and_bit_fields_do_not_raise_the_alignment() {
        let opaque = PlanField {
            offset: 0,
            kind: FieldKind::Opaque {
                span: 8,
                reason: DegradeReason::UnknownType,
            },
        };
        let p = plan(&input(16, 1, vec![opaque, bit(8, 0, 0x01)]));
        assert_eq!(p.align, 1);
    }

    #[test]
    fn a_larger_declared_alignment_is_kept() {
        let p = plan(&input(32, 16, vec![typed(0, 8, 8)]));
        assert_eq!(p.align, 16);
        assert_eq!(p.declared_align, 16);
    }

    #[test]
    fn declared_alignment_below_the_field_is_raised_not_degraded() {
        let p = plan(&input(16, 4, vec![typed(0, 8, 8)]));
        assert_eq!(p.declared_align, 4);
        assert_eq!(p.align, 8);
        assert!(p.notes.is_empty());
    }

    #[test]
    fn size_clamp_wins_over_the_evidence() {
        let p = plan(&input(0x2C, 8, vec![typed(0, 4, 4), typed(8, 8, 8)]));
        // 0x2C is a multiple of 4 only.
        assert_eq!(p.align, 4);
        assert_eq!(reasons(&p), vec![DegradeReason::AlignmentTooLow]);
        let odd = plan(&input(0x29, 8, vec![]));
        assert_eq!(odd.align, 1);
    }

    #[test]
    fn non_power_of_two_alignment_is_one() {
        assert_eq!(plan(&input(12, 12, vec![])).align, 1);
        assert_eq!(plan(&input(12, 0, vec![])).align, 1);
    }

    #[test]
    fn zero_size_type_has_no_items() {
        let p = plan(&input(0, 8, vec![]));
        assert_eq!(p.align, 8);
        assert!(p.items.is_empty());
    }

    #[test]
    fn field_past_the_end_is_left_out() {
        let p = plan(&input(8, 8, vec![typed(8, 4, 4), typed(4, 8, 8)]));
        assert_eq!(
            reasons(&p)
                .iter()
                .filter(|r| **r == DegradeReason::OutOfBounds)
                .count(),
            2
        );
        assert!(p.items.iter().all(|i| !matches!(i, Item::Typed { .. })));
    }

    #[test]
    fn arrays_use_the_whole_span() {
        let f = PlanField {
            offset: 0,
            kind: FieldKind::Typed {
                size: 4,
                align: 4,
                count: 3,
            },
        };
        let p = plan(&input(16, 4, vec![f]));
        assert_eq!(
            p.items,
            vec![
                Item::Typed {
                    field: 0,
                    offset: 0,
                    count: 3
                },
                Item::Pad { offset: 12, len: 4 },
            ]
        );
    }

    #[test]
    fn bits_sharing_a_byte_are_one_storage_group() {
        let p = plan(&input(
            8,
            4,
            vec![bit(4, 0, 0x01), bit(4, 0, 0x02), typed(0, 4, 4)],
        ));
        let bits: Vec<_> = p
            .items
            .iter()
            .filter_map(|i| match i {
                Item::Bits { offset, len, bits } => Some((*offset, *len, bits.len())),
                _ => None,
            })
            .collect();
        assert_eq!(bits, vec![(4, 1, 2)]);
        assert!(p.notes.is_empty());
    }

    #[test]
    fn bit_outside_its_unit_is_invalid() {
        let p = plan(&input(8, 4, vec![bit(4, 3, 0x01)]));
        assert_eq!(reasons(&p), vec![DegradeReason::InvalidBitfield]);
        assert_eq!(p.omitted.len(), 1);
    }

    #[test]
    fn bit_storage_overlapping_a_field_is_omitted() {
        let p = plan(&input(8, 4, vec![typed(0, 4, 4), bit(2, 0, 0x01)]));
        assert_eq!(reasons(&p), vec![DegradeReason::Overlap]);
    }

    #[test]
    fn base_is_embedded_when_it_fits_before_the_first_field() {
        let mut i = input(16, 8, vec![typed(8, 8, 8)]);
        i.base = Some(BaseInfo { size: 8, align: 8 });
        let p = plan(&i);
        assert_eq!(p.base, BaseMode::Embedded);
        assert_eq!(
            p.items[0],
            Item::Typed {
                field: 0,
                offset: 8,
                count: 1
            }
        );
    }

    #[test]
    fn base_larger_than_the_first_field_offset_falls_back_to_a_cast() {
        // The parent ends at 0x10 but the first field is at 0x0C: tail padding reuse.
        let mut i = input(0x18, 8, vec![typed(0x0C, 4, 4)]);
        i.base = Some(BaseInfo {
            size: 0x10,
            align: 8,
        });
        let p = plan(&i);
        assert!(matches!(
            p.base,
            BaseMode::Cast(DegradeReason::BaseNotEmbedded, _)
        ));
        assert_eq!(
            p.items[0],
            Item::Pad {
                offset: 0,
                len: 0x0C
            }
        );
        assert_eq!(reasons(&p), vec![DegradeReason::BaseNotEmbedded]);
    }

    #[test]
    fn empty_base_larger_than_the_type_is_unavailable() {
        let mut i = input(4, 4, vec![]);
        i.base = Some(BaseInfo { size: 8, align: 4 });
        assert!(matches!(plan(&i).base, BaseMode::Unavailable(..)));
        // The size clamp lowers the alignment below the parent's.
        let mut j = input(12, 4, vec![]);
        j.base = Some(BaseInfo { size: 8, align: 8 });
        let p = plan(&j);
        assert_eq!(p.align, 4);
        assert!(matches!(p.base, BaseMode::Unavailable(..)));
    }

    #[test]
    fn opaque_field_is_bytes_without_a_second_note() {
        let f = PlanField {
            offset: 0,
            kind: FieldKind::Opaque {
                span: 8,
                reason: DegradeReason::UnknownType,
            },
        };
        let p = plan(&input(8, 8, vec![f]));
        assert_eq!(
            p.items,
            vec![Item::Bytes {
                field: 0,
                offset: 0,
                len: 8
            }]
        );
        assert!(p.notes.is_empty());
    }

    #[test]
    fn every_byte_is_covered_exactly_once() {
        let fields = vec![
            typed(0, 1, 1),
            typed(3, 2, 2),
            bit(8, 0, 0x04),
            typed(12, 8, 8),
            typed(14, 2, 2),
            typed(40, 8, 8),
        ];
        let input = input(48, 8, fields);
        let p = plan(&input);
        let size_of = |field: usize| match input.fields.get(field).map(|f| &f.kind) {
            Some(FieldKind::Typed { size, count, .. }) => size * count,
            _ => 0,
        };
        let mut cover = 0u32;
        for item in &p.items {
            let (offset, len) = match item {
                Item::Pad { offset, len }
                | Item::Bytes { offset, len, .. }
                | Item::Bits { offset, len, .. } => (*offset, *len),
                Item::Typed { field, offset, .. } => (*offset, size_of(*field)),
            };
            assert_eq!(offset, cover, "gap or overlap at {offset:#x}");
            cover += len;
        }
        assert_eq!(cover, 48);
    }
}
