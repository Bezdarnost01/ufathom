//! Step 4, second half: `UClass`, `UFunction` and `UEnum`.
//!
//! - `ClassDefaultObject` points at the object named `Default__<Class>`.
//! - `ClassCastFlags` contains the cast bits of the super class, and the bit
//!   `Actor` adds over `Object` is present in every class that derives from
//!   `Actor` and in no other.
//! - `FunctionFlags` is the 32-bit field where `ReceiveBeginPlay` is a
//!   Blueprint event and not native, and `K2_DestroyActor` is native and
//!   Blueprint callable.
//! - `Func` is a pointer into an image that differs for every native function.
//! - `UEnum`'s names are a `TArray` whose elements decode to names that carry
//!   the enum's own name as prefix.

use std::collections::{HashMap, HashSet};

use ufathom_model::flags::{
    CPF_OUT_PARM, FUNC_BLUEPRINT_CALLABLE, FUNC_BLUEPRINT_EVENT, FUNC_HAS_OUT_PARMS, FUNC_NATIVE,
};
use ufathom_model::layout::{UClassLayout, UEnumLayout, UFunctionLayout};

use crate::agree::agrees;
use crate::ctx::{Ctx, le_u64};
use crate::error::ScanError;
use crate::limits::{FIELD_AGREEMENT_PERCENT, MAX_ENUM_PROBE, MAX_ENUM_STRIDE};
use crate::props::{Env, Prop, Role};
use crate::structs::derives_from;
use crate::universe::Obj;

/// Share of enums, in percent, whose names must all decode at a stride.
const ENUM_DECODE_PERCENT: usize = 90;

/// Share of enums, in percent, whose names must mostly carry the enum's name
/// as prefix. A real game has a few hundred to a thousand enums, a fifth of
/// them declared without a namespace; a wrong field decodes none.
const ENUM_PREFIX_PERCENT: usize = 60;

/// Fewest classes that must show a relation for it to count.
const MIN_CLASSES: usize = 5;

fn offsets_8(from: u32, to: u32) -> impl Iterator<Item = u32> {
    (from.next_multiple_of(8)..to).step_by(8)
}

fn offsets_4(from: u32, to: u32) -> impl Iterator<Item = u32> {
    (from..to).step_by(4)
}

fn classes(env: &Env<'_, '_>) -> Vec<Obj> {
    env.universe
        .objs
        .iter()
        .filter(|o| o.class == env.anchors.class)
        .copied()
        .collect()
}

/// `ClassDefaultObject`.
fn solve_default_object(env: &Env<'_, '_>, classes: &[Obj]) -> Result<Option<u32>, ScanError> {
    let ctx = env.ctx;
    let limit = env.sizes.class;
    let mut found = Vec::new();
    for offset in offsets_8(0x18, limit.saturating_sub(7)) {
        let mut non_null = 0usize;
        let mut matched = 0usize;
        for class in classes {
            let Some(value) = ctx.u64(class.addr + u64::from(offset)).filter(|&v| v != 0) else {
                continue;
            };
            non_null += 1;
            let wanted = env
                .universe
                .name(env.table, class)
                .map(|n| format!("Default__{n}"));
            let got = env
                .universe
                .get(value)
                .and_then(|o| env.universe.name(env.table, o));
            if let (Some(wanted), Some(got)) = (wanted, got)
                && wanted.eq_ignore_ascii_case(got)
            {
                matched += 1;
            }
        }
        if non_null >= MIN_CLASSES && matched * 100 >= non_null * 85 {
            found.push((offset, matched, non_null));
        }
    }
    let solved = match found.as_slice() {
        [(offset, _, _)] => Some(*offset),
        _ => None,
    };
    let result = ctx.resolve_optional("uclass.default_object", solved)?;
    match (result, found.first()) {
        (Some(offset), Some(&(_, matched, total))) if solved.is_some() => ctx.evidence(
            "uclass.default_object",
            u64::from(offset),
            "points at the object named Default__<ClassName>",
            total,
            matched,
        ),
        (None, _) => {
            ctx.warn("uclass.default_object: no unique pointer leads to the Default__ object")
        }
        _ => {}
    }
    Ok(result)
}

/// `ClassCastFlags`.
fn solve_cast_flags(
    env: &Env<'_, '_>,
    classes: &[Obj],
    default_object: Option<u32>,
    ustruct_end: u32,
) -> Result<Option<u32>, ScanError> {
    let ctx = env.ctx;
    let (Some(&actor), Some(&object)) =
        (env.anchors.engine.get("actor"), Some(&env.anchors.object))
    else {
        ctx.warn("uclass.cast_flags: no Actor class to anchor the cast bits on");
        return ctx.resolve_optional("uclass.cast_flags", None);
    };
    let super_offset = env.fields.ustruct.super_struct;
    let pairs: Vec<(u64, u64)> = classes
        .iter()
        .filter_map(|c| {
            let parent = ctx.u64(c.addr + u64::from(super_offset))?;
            (parent != 0).then_some((c.addr, parent))
        })
        .collect();
    let actor_family: HashSet<u64> = classes
        .iter()
        .filter(|c| derives_from(ctx, super_offset, c.addr, actor))
        .map(|c| c.addr)
        .collect();
    let mut found = Vec::new();
    // `UClass` adds its fields after `UStruct`, and so does `UFunction`, whose
    // first field is `FunctionFlags`: nothing of `UClass` lies below it.
    for offset in offsets_8(ustruct_end, env.sizes.class.saturating_sub(7)) {
        if Some(offset) == default_object || offset == super_offset {
            continue;
        }
        let read = |addr: u64| ctx.u64(addr + u64::from(offset));
        let nested = pairs
            .iter()
            .filter(|&&(child, parent)| match (read(child), read(parent)) {
                (Some(c), Some(p)) => c & p == p,
                _ => false,
            })
            .count();
        if pairs.len() < MIN_CLASSES || !agrees(nested, pairs.len(), FIELD_AGREEMENT_PERCENT) {
            continue;
        }
        let (Some(actor_flags), Some(object_flags)) = (read(actor), read(object)) else {
            continue;
        };
        // `UObject` is the root of the hierarchy and casts to nothing but
        // itself: its flags are zero, which `ClassFlags` and the like are not.
        let bits = actor_flags & !object_flags;
        if bits == 0 || object_flags != 0 {
            continue;
        }
        let consistent = classes
            .iter()
            .filter(|c| {
                read(c.addr).is_some_and(|f| (f & bits == bits) == actor_family.contains(&c.addr))
            })
            .count();
        if agrees(consistent, classes.len(), FIELD_AGREEMENT_PERCENT) {
            found.push((offset, nested, pairs.len()));
        }
    }
    let solved = match found.as_slice() {
        [(offset, _, _)] => Some(*offset),
        _ => None,
    };
    let result = ctx.resolve_optional("uclass.cast_flags", solved)?;
    match (result, found.first()) {
        (Some(offset), Some(&(_, nested, total))) if solved.is_some() => ctx.evidence(
            "uclass.cast_flags",
            u64::from(offset),
            "each class has its super's bits, and the bit Actor adds is in Actor's descendants only",
            total,
            nested,
        ),
        (None, _) => ctx.warn(format!(
            "uclass.cast_flags: {} candidate offsets fit ({}); none is chosen",
            found.len(),
            found
                .iter()
                .map(|&(offset, _, _)| format!("{offset:#x}"))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        _ => {}
    }
    Ok(result)
}

/// The members of `Actor` by name.
fn actor_functions(env: &Env<'_, '_>) -> HashMap<String, u64> {
    let mut out = HashMap::new();
    let Some(&actor) = env.anchors.engine.get("actor") else {
        return out;
    };
    for function in env
        .universe
        .objs
        .iter()
        .filter(|o| o.class == env.anchors.function && o.outer == actor)
    {
        if let Some(name) = env.universe.name(env.table, function) {
            out.insert(name.to_ascii_lowercase(), function.addr);
        }
    }
    out
}

/// `FunctionFlags`.
fn solve_function_flags(
    env: &Env<'_, '_>,
    props: &[Prop],
    flags_offset_hint: u32,
) -> Result<u32, ScanError> {
    let ctx = env.ctx;
    let named = actor_functions(env);
    let (Some(&begin_play), Some(&destroy)) =
        (named.get("receivebeginplay"), named.get("k2_destroyactor"))
    else {
        return ctx.resolve("ufunction.flags", None, || {
            "Actor has no ReceiveBeginPlay and K2_DestroyActor to anchor the flags on".to_owned()
        });
    };
    let functions: Vec<u64> = env
        .universe
        .objs
        .iter()
        .filter(|o| o.class == env.anchors.function)
        .map(|o| o.addr)
        .collect();
    let mut found: Vec<u32> = offsets_4(0x18, env.sizes_function().saturating_sub(3))
        .filter(|&offset| {
            let read = |addr: u64| ctx.u32(addr + u64::from(offset));
            let event_ok = read(begin_play)
                .is_some_and(|f| f & FUNC_BLUEPRINT_EVENT != 0 && f & FUNC_NATIVE == 0);
            let native_ok = read(destroy).is_some_and(|f| {
                f & (FUNC_NATIVE | FUNC_BLUEPRINT_CALLABLE) == FUNC_NATIVE | FUNC_BLUEPRINT_CALLABLE
            });
            event_ok && native_ok
        })
        .collect();
    let mut method = "ReceiveBeginPlay is a Blueprint event and not native, K2_DestroyActor is native and Blueprint callable".to_owned();
    if found.len() > 1 {
        // Several fields satisfy the two anchors. The flag that says a
        // function has out parameters must agree with its parameters: take
        // the field where it does for the most functions.
        let has_out: HashMap<u64, bool> = functions
            .iter()
            .map(|&f| {
                let out = props.iter().any(|p| {
                    p.owner == f && p.role == Role::Param && p.flags_has_out(ctx, flags_offset_hint)
                });
                (f, out)
            })
            .collect();
        let score = |offset: u32| {
            functions
                .iter()
                .filter(|&&f| {
                    ctx.u32(f + u64::from(offset)).is_some_and(|v| {
                        (v & FUNC_HAS_OUT_PARMS != 0) == has_out.get(&f).copied().unwrap_or(false)
                    })
                })
                .count()
        };
        let mut scored: Vec<(u32, usize)> = found.iter().map(|&o| (o, score(o))).collect();
        scored.sort_by_key(|&(o, s)| (std::cmp::Reverse(s), o));
        if let [(offset, best), rest @ ..] = scored.as_slice()
            && rest.first().is_none_or(|r| r.1 < *best)
            && *best * 100 >= functions.len() * 95
        {
            found = vec![*offset];
            method.push_str("; HasOutParms agrees with the out parameters of the most functions");
        }
    }
    let offset = ctx.resolve_among("ufunction.flags", &found)?;
    ctx.evidence("ufunction.flags", u64::from(offset), method, 2, 2);
    Ok(offset)
}

impl Env<'_, '_> {
    /// `sizeof(UFunction)`: the `PropertiesSize` of the `Function` class.
    pub(crate) fn sizes_function(&self) -> u32 {
        self.ctx
            .u32(self.anchors.function + u64::from(self.fields.ustruct.properties_size))
            .unwrap_or(0)
    }
}

impl Prop {
    fn flags_has_out(&self, ctx: &Ctx<'_>, flags_offset: u32) -> bool {
        ctx.u64(self.addr + u64::from(flags_offset))
            .is_some_and(|f| f & CPF_OUT_PARM != 0)
    }
}

/// `UFunction::Func`.
fn solve_native_func(env: &Env<'_, '_>, flags: u32) -> Result<Option<u32>, ScanError> {
    let ctx = env.ctx;
    let natives: Vec<u64> = env
        .universe
        .objs
        .iter()
        .filter(|o| o.class == env.anchors.function)
        .map(|o| o.addr)
        .filter(|&f| {
            ctx.u32(f + u64::from(flags))
                .is_some_and(|v| v & FUNC_NATIVE != 0)
        })
        .collect();
    let mut found = Vec::new();
    for offset in offsets_8(flags + 4, env.sizes_function().saturating_sub(7)) {
        let values: Vec<u64> = natives
            .iter()
            .filter_map(|&f| ctx.u64(f + u64::from(offset)))
            .collect();
        let in_image = values.iter().filter(|&&v| ctx.in_image(v)).count();
        let distinct: HashSet<u64> = values.iter().copied().collect();
        if natives.len() >= MIN_CLASSES
            && in_image * 100 >= natives.len() * 95
            && distinct.len() * 2 >= natives.len()
        {
            found.push((offset, in_image));
        }
    }
    let solved = match found.as_slice() {
        [(offset, _)] => Some(*offset),
        _ => None,
    };
    let result = ctx.resolve_optional("ufunction.native_func", solved)?;
    match (result, found.first()) {
        (Some(offset), Some(&(_, hits))) if solved.is_some() => ctx.evidence(
            "ufunction.native_func",
            u64::from(offset),
            "points into an image, differently for each native function",
            natives.len(),
            hits,
        ),
        (None, _) => ctx.warn(format!(
            "ufunction.native_func: {} candidate offsets point into an image for the native functions; none is chosen",
            found.len()
        )),
        _ => {}
    }
    Ok(result)
}

/// The classes, functions and flags that `UClass` and `UFunction` add.
pub(crate) struct Members {
    pub uclass: UClassLayout,
    pub ufunction: UFunctionLayout,
}

/// Solves `UClass` and `UFunction`.
pub(crate) fn solve_members(
    env: &Env<'_, '_>,
    props: &[Prop],
    property_flags: u32,
) -> Result<Members, ScanError> {
    let classes = classes(env);
    let default_object = solve_default_object(env, &classes)?;
    let flags = solve_function_flags(env, props, property_flags)?;
    let cast_flags = solve_cast_flags(env, &classes, default_object, flags)?;
    let native_func = solve_native_func(env, flags)?;
    Ok(Members {
        uclass: UClassLayout {
            cast_flags,
            default_object,
        },
        ufunction: UFunctionLayout { flags, native_func },
    })
}

// ----- enums -----

struct EnumSample {
    addr: u64,
    name: String,
}

fn enum_samples(env: &Env<'_, '_>) -> Vec<EnumSample> {
    env.universe
        .objs
        .iter()
        .filter(|o| o.class == env.anchors.enum_)
        .filter_map(|o| {
            Some(EnumSample {
                addr: o.addr,
                name: env.universe.name(env.table, o)?.to_owned(),
            })
        })
        .collect()
}

fn has_prefix(text: &str, enum_name: &str) -> bool {
    text.split_once("::")
        .is_some_and(|(prefix, rest)| !rest.is_empty() && prefix.eq_ignore_ascii_case(enum_name))
}

/// The number of consecutive prefixed names that start at `data` with the
/// given stride, up to `limit`.
fn name_run(env: &Env<'_, '_>, data: u64, stride: u32, name: &str, limit: u32) -> (u32, u32) {
    let mut run = 0;
    let mut prefixed = 0;
    while run < limit {
        let at = data + u64::from(run) * u64::from(stride);
        let Some(text) = env.fname_at(at) else {
            break;
        };
        run += 1;
        prefixed += u32::from(has_prefix(&text, name));
    }
    (run, prefixed)
}

/// The number of consecutive elements, from `data`, whose names all carry the
/// enum's name as prefix. Used where the array is delimited by that alone.
fn prefixed_run(env: &Env<'_, '_>, data: u64, stride: u32, name: &str, limit: u32) -> u32 {
    let mut count = 0;
    while count < limit {
        let at = data + u64::from(count) * u64::from(stride);
        match env.fname_at(at) {
            Some(text) if has_prefix(&text, name) => count += 1,
            _ => break,
        }
    }
    count
}

/// Whether most of the names of a run carry the enum's name as prefix. Not
/// all: enums declared without a namespace store bare enumerator names, and
/// they are a fifth of a real game's enums.
fn mostly_prefixed((run, prefixed): (u32, u32)) -> bool {
    // Four in five, not half: read at half the true stride, the values between
    // the names decode as the name `None`, and half the elements then carry
    // the prefix.
    run > 0 && prefixed * 5 >= run * 4
}

/// Values of the elements of a pair array: little-endian, `width` bytes at
/// `value_offset`.
fn read_values(
    ctx: &Ctx<'_>,
    data: u64,
    stride: u32,
    count: u32,
    value_offset: u32,
    width: u32,
) -> Option<Vec<u64>> {
    let bytes = ctx.read(data, (stride * count) as usize)?;
    (0..count)
        .map(|k| {
            let at = (k * stride + value_offset) as usize;
            let raw = bytes.get(at..at + width as usize)?;
            let mut buf = [0u8; 8];
            buf.get_mut(..raw.len())?.copy_from_slice(raw);
            Some(u64::from_le_bytes(buf))
        })
        .collect()
}

/// Whether enumerator values ascend from a small first value and fit 32
/// bits. The bound rejects a read that starts inside the padding before the
/// value, whose results are the real values shifted up by 32 bits.
fn ascending(values: &[u64]) -> bool {
    values.windows(2).all(|w| matches!(w, [a, b] if a < b))
        && values.first().is_some_and(|&v| v < 0x1_0000)
        && values.iter().all(|&v| v <= u64::from(u32::MAX))
}

/// Whether the engine this image comes from has `EnumProperty`, which came
/// with 64-bit enum values.
fn has_enum_property(env: &Env<'_, '_>, props: &[Prop]) -> bool {
    props.iter().any(|p| p.class_name == "EnumProperty")
        || env
            .universe
            .named(env.table, &["EnumProperty"])
            .values()
            .flatten()
            .any(|&i| {
                env.universe
                    .objs
                    .get(i)
                    .is_some_and(|o| o.class == env.anchors.class)
            })
}

fn solve_pair_array(
    env: &Env<'_, '_>,
    enums: &[EnumSample],
    props: &[Prop],
) -> Option<UEnumLayout> {
    let ctx = env.ctx;
    let limit = ctx
        .u32(env.anchors.enum_ + u64::from(env.fields.ustruct.properties_size))
        .unwrap_or(0);
    for offset in offsets_8(0x18, limit.saturating_sub(15)) {
        // The array header of every enum: pointer, num, max.
        let arrays: Vec<(&EnumSample, u64, u32)> = enums
            .iter()
            .filter_map(|e| {
                let data = ctx.u64(e.addr + u64::from(offset))?;
                let num = ctx.u32(e.addr + u64::from(offset) + 8)?;
                let max = ctx.u32(e.addr + u64::from(offset) + 12)?;
                (num > 0 && num <= max && max <= MAX_ENUM_PROBE && ctx.is_pointer(data))
                    .then_some((e, data, num))
            })
            .collect();
        if arrays.len() * 2 < enums.len() || arrays.len() < 2 {
            continue;
        }
        let min_stride = env.fname.size.max(4);
        for stride in (min_stride..=MAX_ENUM_STRIDE).step_by(4) {
            let runs: Vec<(u32, u32, u32)> = arrays
                .iter()
                .map(|&(e, data, num)| {
                    let (run, prefixed) = name_run(env, data, stride, &e.name, num);
                    (num, run, prefixed)
                })
                .collect();
            let decoded = runs.iter().filter(|&&(num, run, _)| run == num).count();
            let good = runs
                .iter()
                .filter(|&&(num, run, prefixed)| run == num && mostly_prefixed((run, prefixed)))
                .count();
            tracing::trace!(
                offset,
                stride,
                arrays = arrays.len(),
                decoded,
                good,
                "enum names"
            );
            // Every element must decode, and the names must belong to the
            // enum: most of the arrays carry its name as prefix.
            let ok = good > 0
                && decoded * 100 >= arrays.len() * ENUM_DECODE_PERCENT
                && good * 100 >= arrays.len() * ENUM_PREFIX_PERCENT;
            if !ok {
                continue;
            }
            return pair_value(env, &arrays, offset, stride, props);
        }
    }
    None
}

fn pair_value(
    env: &Env<'_, '_>,
    arrays: &[(&EnumSample, u64, u32)],
    offset: u32,
    stride: u32,
    props: &[Prop],
) -> Option<UEnumLayout> {
    let ctx = env.ctx;
    let name_size = env.fname.size;
    for value_offset in (name_size..stride).step_by(1) {
        let widths: Vec<u32> = [1u32, 2, 4, 8]
            .into_iter()
            .filter(|&w| value_offset + w <= stride && value_offset % w == 0)
            .filter(|&w| {
                let good = arrays
                    .iter()
                    .filter(|&&(_, data, num)| {
                        read_values(ctx, data, stride, num, value_offset, w)
                            .is_some_and(|v| ascending(&v))
                    })
                    .count();
                good * 100 >= arrays.len() * 70
            })
            .collect();
        if widths.is_empty() {
            continue;
        }
        // Values that fit a narrow field read the same through a wider one.
        // The engine widened enum values to 64 bits with `EnumProperty`.
        let wide = has_enum_property(env, props);
        let value_width = if wide {
            widths.iter().copied().max()?
        } else {
            widths.iter().copied().min()?
        };
        return Some(UEnumLayout::PairArray {
            offset,
            stride,
            value_offset,
            value_width,
        });
    }
    None
}

fn solve_split_arrays(env: &Env<'_, '_>, enums: &[EnumSample]) -> Option<UEnumLayout> {
    let ctx = env.ctx;
    let limit = ctx
        .u32(env.anchors.enum_ + u64::from(env.fields.ustruct.properties_size))
        .unwrap_or(0);
    let tag = 0x7u64;
    let name_size = env.fname.size;
    for names in offsets_8(0x18, limit.saturating_sub(7)) {
        let runs: Vec<(&EnumSample, u64, u32)> = enums
            .iter()
            .filter_map(|e| {
                let data = ctx.u64(e.addr + u64::from(names))? & !tag;
                (ctx.is_pointer(data)).then_some(())?;
                let run = prefixed_run(env, data, name_size, &e.name, MAX_ENUM_PROBE);
                (run >= 2).then_some((e, data, run))
            })
            .collect();
        if runs.len() * 2 < enums.len() || runs.len() < 2 {
            continue;
        }
        for count in offsets_4(0x18, limit.saturating_sub(3)) {
            let counted = runs
                .iter()
                .all(|&(e, _, run)| ctx.u32(e.addr + u64::from(count)) == Some(run));
            if !counted {
                continue;
            }
            for values in offsets_8(0x18, limit.saturating_sub(7)).filter(|&o| o != names) {
                let ok = runs
                    .iter()
                    .filter(|&&(e, _, run)| {
                        let Some(data) = ctx.u64(e.addr + u64::from(values)).map(|v| v & !tag)
                        else {
                            return false;
                        };
                        let Some(bytes) = ctx.read(data, run as usize * 8) else {
                            return false;
                        };
                        let parsed: Vec<u64> = (0..run as usize)
                            .filter_map(|k| le_u64(&bytes, k * 8))
                            .collect();
                        ascending(&parsed)
                    })
                    .count();
                if ok * 100 >= runs.len() * 70 {
                    return Some(UEnumLayout::SplitArrays {
                        names,
                        values,
                        count,
                    });
                }
            }
        }
    }
    None
}

/// Solves how `UEnum` stores its names and values.
pub(crate) fn solve_enum(env: &Env<'_, '_>, props: &[Prop]) -> Result<UEnumLayout, ScanError> {
    let ctx = env.ctx;
    let enums = enum_samples(env);
    let solved = solve_pair_array(env, &enums, props).or_else(|| solve_split_arrays(env, &enums));
    let Some(layout) = solved else {
        return Err(ScanError::missing(
            "uenum",
            format!(
                "no TArray-shaped field of the {} enums decodes to names carrying the enum's own name as prefix, \
                 in the pair form or the split form",
                enums.len()
            ),
        ));
    };
    // Overrides: the layout is one value; check its fields one by one.
    match &layout {
        UEnumLayout::PairArray {
            offset,
            stride,
            value_offset,
            value_width,
        } => {
            ctx.evidence(
                "uenum.offset",
                u64::from(*offset),
                "TArray whose elements decode to names prefixed with the enum's name",
                enums.len(),
                enums.len(),
            );
            ctx.evidence(
                "uenum.stride",
                u64::from(*stride),
                "the stride at which every element's name decodes",
                enums.len(),
                enums.len(),
            );
            ctx.evidence(
                "uenum.value_offset",
                u64::from(*value_offset),
                "first offset after the name where the values ascend",
                enums.len(),
                enums.len(),
            );
            ctx.evidence(
                "uenum.value_width",
                u64::from(*value_width),
                "widest field that reads the same values; 64-bit once EnumProperty exists, else the narrowest",
                enums.len(),
                enums.len(),
            );
        }
        UEnumLayout::SplitArrays {
            names,
            values,
            count,
        } => {
            for (field, value) in [
                ("uenum.names", names),
                ("uenum.values", values),
                ("uenum.count", count),
            ] {
                ctx.evidence(
                    field,
                    u64::from(*value),
                    "tagged pointers to names and values with a shared count",
                    enums.len(),
                    enums.len(),
                );
            }
        }
    }
    Ok(layout)
}
