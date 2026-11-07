//! Typed property payloads, solved without anchors.
//!
//! For each property kind, its instances are sampled and the offsets past the
//! core members are scanned. The first offset where the values of (nearly)
//! every instance resolve to an object of the expected kind is the payload:
//! a `UClass` for object properties, a `UScriptStruct` for struct properties,
//! a `UEnum` for byte and enum properties, a `UFunction` for delegates, and a
//! property owned by the instance for arrays, sets, maps and optionals.

use ufathom_engine::PropertyKind;
use ufathom_model::layout::{BoolPayloadLayout, TypedPayloadLayout};

use crate::agree::agrees;
use crate::error::ScanError;
use crate::limits::{MAX_PAYLOAD_SAMPLES, MAX_PROPERTY_OFFSET, PAYLOAD_AGREEMENT_PERCENT};
use crate::structs::derives_from;

use super::core::Core;
use super::{Env, Prop, System};

/// Offsets searched past the core members, in bytes.
const PAYLOAD_SPAN: u32 = 0x60;

/// What the typed solve found.
pub(crate) struct Typed {
    pub layout: TypedPayloadLayout,
}

fn kind_of(prop: &Prop) -> PropertyKind {
    PropertyKind::from_class_name(&prop.class_name)
}

/// Instances of the given kinds, spread over the whole set.
fn instances<'p>(props: &'p [Prop], kinds: &[PropertyKind]) -> Vec<&'p Prop> {
    let all: Vec<&Prop> = props
        .iter()
        .filter(|p| kinds.contains(&kind_of(p)))
        .collect();
    let step = (all.len() / MAX_PAYLOAD_SAMPLES).max(1);
    all.into_iter().step_by(step).collect()
}

/// Per-instance predicate: does the value at `offset` have the expected
/// meaning?
type Check<'c> = dyn Fn(&Prop, u32) -> bool + 'c;

/// The first 8-aligned offset from `start` where nearly every instance
/// satisfies `check`, with the number of instances that did.
fn first_offset(instances: &[&Prop], start: u32, check: &Check<'_>) -> Option<(u32, usize)> {
    first_offset_where(instances, start, check, &|_| true)
}

/// Like [`first_offset`], but an offset must also pass `accept`, which sees
/// the offset once the agreement bar is met.
fn first_offset_where(
    instances: &[&Prop],
    start: u32,
    check: &Check<'_>,
    accept: &dyn Fn(u32) -> bool,
) -> Option<(u32, usize)> {
    if instances.is_empty() {
        return None;
    }
    let end = start + PAYLOAD_SPAN.max(MAX_PROPERTY_OFFSET);
    (start..end).step_by(8).find_map(|offset| {
        let matched = instances.iter().filter(|p| check(p, offset)).count();
        (agrees(matched, instances.len(), PAYLOAD_AGREEMENT_PERCENT)
            && matched > 0
            && accept(offset))
        .then_some((offset, matched))
    })
}

/// Finds the offset inside an `FField` that holds its owner.
fn solve_owner_offset(env: &Env<'_, '_>, system: &System, props: &[Prop]) -> Option<u32> {
    let System::Modern(modern) = system else {
        return None;
    };
    let members: Vec<&Prop> = props.iter().collect();
    let found: Vec<u32> = (8..0x40u32)
        .step_by(8)
        .filter(|&o| o != modern.next && o != modern.class)
        .filter(|&o| {
            let hits = members
                .iter()
                .filter(|p| env.ctx.u64(p.addr + u64::from(o)) == Some(p.owner))
                .count();
            members.len() >= 8 && agrees(hits, members.len(), PAYLOAD_AGREEMENT_PERCENT)
        })
        .collect();
    match found.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

fn solve_bool(
    env: &Env<'_, '_>,
    instances: &[&Prop],
    start: u32,
) -> Option<(BoolPayloadLayout, usize)> {
    if instances.is_empty() {
        return None;
    }
    let end = start + PAYLOAD_SPAN.max(MAX_PROPERTY_OFFSET);
    for base in start..end {
        let quads: Vec<[u8; 4]> = instances
            .iter()
            .filter_map(|p| {
                let bytes = env.ctx.read(p.addr + u64::from(base), 4)?;
                <[u8; 4]>::try_from(bytes.as_slice()).ok()
            })
            .collect();
        let single = |mask: u8| mask == 0xFF || mask.is_power_of_two();
        let valid = |q: &[u8; 4]| {
            let [size, byte_offset, byte_mask, field_mask] = *q;
            [1u8, 2, 4, 8].contains(&size)
                && byte_offset < size
                && single(byte_mask)
                // A bit-field bool has the same single-bit mask in both; a
                // native `bool` has `ByteMask` 1 and `FieldMask` 0xFF; a bit
                // in a higher byte leaves the low byte of `FieldMask` empty.
                && (byte_offset != 0 || field_mask == byte_mask || field_mask == 0xFF)
        };
        let ok = quads.iter().filter(|q| valid(q)).count();
        let mut masks: Vec<u8> = quads.iter().map(|q| q[2]).collect();
        masks.sort_unstable();
        masks.dedup();
        let varied = masks.len() >= 2 || instances.len() < 4;
        if agrees(ok, instances.len(), PAYLOAD_AGREEMENT_PERCENT) && ok > 0 && varied {
            let at = |extra: u32| base + extra;
            return Some((
                BoolPayloadLayout {
                    field_size: at(0),
                    byte_offset: at(1),
                    byte_mask: at(2),
                    field_mask: at(3),
                },
                ok,
            ));
        }
    }
    None
}

/// Solves every typed payload offset.
#[allow(
    clippy::too_many_lines,
    reason = "one flat table of property kinds, each with its own meaning of 'resolves'"
)]
pub(crate) fn solve_typed(
    env: &Env<'_, '_>,
    system: &System,
    props: &[Prop],
    core: &Core,
) -> Result<Typed, ScanError> {
    let ctx = env.ctx;
    let a = env.anchors;
    let universe = env.universe;
    let start = (core
        .array_dim
        .max(core.element_size)
        .max(core.offset_internal)
        .max(core.flags + 4)
        + 4)
    .next_multiple_of(8);
    let owner_offset = solve_owner_offset(env, system, props);

    // The target is an object of `class` or of a class derived from it:
    // delegate signatures are `DelegateFunction`s, and Blueprint classes are
    // `BlueprintGeneratedClass`es, which derive from `Function` and `Class`.
    let super_offset = env.fields.ustruct.super_struct;
    let is_kind = move |target: u64, class: u64| {
        universe
            .get(target)
            .is_some_and(|o| o.class == class || derives_from(ctx, super_offset, o.class, class))
    };
    let points_to = |class: u64| {
        move |p: &Prop, offset: u32| {
            ctx.u64(p.addr + u64::from(offset))
                .is_some_and(|v| is_kind(v, class))
        }
    };
    let owned_at = |p: &Prop, offset: u32| -> bool {
        let Some(target) = ctx.u64(p.addr + u64::from(offset)).filter(|&t| t != 0) else {
            return false;
        };
        match system {
            System::Legacy => universe.get(target).is_some_and(|o| o.outer == p.addr),
            System::Modern(_) => {
                owner_offset.is_some_and(|o| ctx.u64(target + u64::from(o)) == Some(p.addr))
            }
        }
    };

    let mut found: Vec<(&'static str, Option<u32>, usize)> = Vec::new();
    let mut record = |name: &'static str, result: Option<(u32, usize)>| {
        found.push((name, result.map(|r| r.0), result.map_or(0, |r| r.1)));
        result.map(|r| r.0)
    };

    let object_like = instances(
        props,
        &[
            PropertyKind::Object,
            PropertyKind::WeakObject,
            PropertyKind::LazyObject,
            PropertyKind::SoftObject,
        ],
    );
    let object_class = record(
        "typed.object_class",
        first_offset(&object_like, start, &points_to(a.class)),
    );
    let class_like = instances(props, &[PropertyKind::Class, PropertyKind::SoftClass]);
    let class_meta = object_class.and_then(|base| {
        record(
            "typed.class_meta",
            first_offset(&class_like, base + 8, &points_to(a.class)),
        )
    });
    let script_struct = record(
        "typed.script_struct",
        first_offset(
            &instances(props, &[PropertyKind::Struct]),
            start,
            &points_to(a.script_struct),
        ),
    );
    let array_inner = record(
        "typed.array_inner",
        first_offset(&instances(props, &[PropertyKind::Array]), start, &owned_at),
    );
    let set_element = record(
        "typed.set_element",
        first_offset(&instances(props, &[PropertyKind::Set]), start, &owned_at),
    );
    let map_instances = instances(props, &[PropertyKind::Map]);
    let map_key = record(
        "typed.map_key",
        first_offset(&map_instances, start, &|p, offset| {
            owned_at(p, offset) && owned_at(p, offset + 8)
        }),
    );
    let map_value = map_key.map(|k| k + 8);
    let enum_instances = instances(props, &[PropertyKind::Enum]);
    let enum_underlying = record(
        "typed.enum_underlying",
        first_offset(&enum_instances, start, &owned_at),
    );
    let enum_type = enum_underlying.and_then(|base| {
        record(
            "typed.enum_type",
            first_offset(&enum_instances, base + 8, &points_to(a.enum_)),
        )
    });
    // A byte property holds an enum or null; at least one must hold an enum.
    let byte_instances = instances(props, &[PropertyKind::Byte]);
    let is_enum = |p: &Prop, offset: u32| {
        ctx.u64(p.addr + u64::from(offset))
            .is_some_and(|v| is_kind(v, a.enum_))
    };
    let byte_enum = record(
        "typed.byte_enum",
        first_offset_where(
            &byte_instances,
            start,
            &|p, offset| ctx.u64(p.addr + u64::from(offset)) == Some(0) || is_enum(p, offset),
            &|offset| byte_instances.iter().any(|p| is_enum(p, offset)),
        ),
    );
    let delegate_signature = record(
        "typed.delegate_signature",
        first_offset(
            &instances(
                props,
                &[
                    PropertyKind::Delegate,
                    PropertyKind::MulticastInlineDelegate,
                    PropertyKind::MulticastSparseDelegate,
                ],
            ),
            start,
            &points_to(a.function),
        ),
    );
    let interface_class = record(
        "typed.interface_class",
        first_offset(
            &instances(props, &[PropertyKind::Interface]),
            start,
            &points_to(a.class),
        ),
    );
    let optional_value = record(
        "typed.optional_value",
        first_offset(
            &instances(props, &[PropertyKind::Optional]),
            start,
            &owned_at,
        ),
    );
    let bool_result = solve_bool(env, &instances(props, &[PropertyKind::Bool]), start);
    if let Some((layout, ok)) = &bool_result {
        found.push(("typed.bool_property", Some(layout.field_size), *ok));
    }

    for &(name, offset, matched) in &found {
        if let Some(offset) = offset {
            ctx.evidence(
                name,
                u64::from(offset),
                "first offset past the core members where the instances' values resolve to the expected kind of object",
                matched,
                matched,
            );
        }
    }
    let missing: Vec<&str> = found
        .iter()
        .filter(|&&(_, offset, _)| offset.is_none())
        .map(|&(name, _, _)| name)
        .collect();
    if !missing.is_empty() {
        ctx.warn(format!(
            "typed payloads without evidence (no instance in the image, or none resolved): {}",
            missing.join(", ")
        ));
    }
    Ok(Typed {
        layout: TypedPayloadLayout {
            bool_property: bool_result.map(|(layout, _)| layout),
            object_class,
            class_meta,
            script_struct,
            array_inner,
            set_element,
            map_key,
            map_value,
            enum_underlying,
            enum_type,
            byte_enum,
            delegate_signature,
            interface_class,
            optional_value,
        },
    })
}

/// Offsets of the typed payloads that sit at the start of the payload area.
fn payload_offsets(typed: &TypedPayloadLayout) -> Vec<u32> {
    [
        typed.object_class,
        typed.script_struct,
        typed.set_element,
        typed.map_key,
        typed.enum_underlying,
        typed.byte_enum,
        typed.delegate_signature,
        typed.interface_class,
        typed.array_inner,
        typed.bool_property.as_ref().map(|b| b.field_size),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The size of the property base: where typed payloads start.
pub(crate) fn solve_size(
    env: &Env<'_, '_>,
    system: &System,
    core: &Core,
    typed: &Typed,
) -> Result<u32, ScanError> {
    let ctx = env.ctx;
    let offsets = payload_offsets(&typed.layout);
    let lowest = offsets.iter().copied().min();
    let (solved, method) = match system {
        System::Legacy => {
            // The `Property` class records sizeof(UProperty) itself.
            let class = env
                .universe
                .named(env.table, &["Property"])
                .remove("property")
                .and_then(|list| {
                    list.into_iter()
                        .filter_map(|i| env.universe.objs.get(i))
                        .find(|o| o.class == env.anchors.class)
                        .map(|o| o.addr)
                });
            (
                class.and_then(|c| ctx.u32(c + u64::from(env.fields.ustruct.properties_size))),
                "PropertiesSize of the Property class; typed payloads start at or after it",
            )
        }
        System::Modern(_) => (
            lowest,
            "the lowest typed payload offset: where the base ends and payloads begin",
        ),
    };
    let size = ctx.resolve("properties.size", solved, || {
        "no typed payload offset was found and the Property class is not readable".to_owned()
    })?;
    let core_end = core
        .array_dim
        .max(core.element_size)
        .max(core.offset_internal)
        .max(core.flags + 4)
        + 4;
    let violations: Vec<u32> = offsets.iter().copied().filter(|&o| o < size).collect();
    if !violations.is_empty() || size < core_end {
        return Err(ScanError::ValidationFailed {
            invariant: "typed payloads start at or after the property base",
            checked: offsets.len(),
            violations: violations.len().max(usize::from(size < core_end)),
            example: format!(
                "properties.size {size:#x}, core members end at {core_end:#x}, lowest payload {:#x}",
                lowest.unwrap_or(0)
            ),
        });
    }
    ctx.evidence(
        "properties.size",
        u64::from(size),
        method,
        offsets.len(),
        offsets.len(),
    );
    Ok(size)
}

/// `sizeof(FName)`: the element size of a `NameProperty`.
pub(crate) fn solve_fname_size(
    env: &Env<'_, '_>,
    props: &[Prop],
    core: &Core,
) -> Result<u32, ScanError> {
    let ctx = env.ctx;
    let element_size = core.element_size;
    let mut sizes: Vec<u32> = props
        .iter()
        .filter(|p| kind_of(p) == PropertyKind::Name)
        .filter_map(|p| ctx.u32(p.addr + u64::from(element_size)))
        .collect();
    sizes.sort_unstable();
    let total = sizes.len();
    let mode = {
        let mut best = (0usize, 0u32);
        let mut i = 0;
        while i < sizes.len() {
            let value = sizes.get(i).copied().unwrap_or(0);
            let run = sizes.iter().filter(|&&s| s == value).count();
            if run > best.0 {
                best = (run, value);
            }
            i += run.max(1);
        }
        best
    };
    let solved = (total > 0 && [4, 8, 12, 16].contains(&mode.1) && mode.1 >= env.fname.size)
        .then_some(mode.1);
    if total > 0 && solved.is_none() {
        return Err(ScanError::ValidationFailed {
            invariant: "a NameProperty is as large as the FName fields found in objects",
            checked: total,
            violations: total,
            example: format!(
                "NameProperty element size {:#x}, FName fields span {:#x}",
                mode.1, env.fname.size
            ),
        });
    }
    let size = ctx.resolve("fname.size", solved.or(Some(env.fname.size)), String::new)?;
    match solved {
        Some(_) => ctx.evidence(
            "fname.size",
            u64::from(size),
            "element size of NameProperty members",
            total,
            mode.0,
        ),
        None => ctx.warn(format!(
            "fname.size = {size:#x} is the span of the fields found in objects: no NameProperty \
             exists to confirm trailing padding"
        )),
    }
    Ok(size)
}
