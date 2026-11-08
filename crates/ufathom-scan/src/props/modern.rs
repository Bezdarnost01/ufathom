//! Telling the property systems apart, and the `FField` offsets.
//!
//! `child_properties`, `FField::Next`, `FField::Name`, `FField::Class` and
//! `FFieldClass::Name` are found together: a pointer in a script struct that
//! is not an object, whose chain through some `Next` offset yields the known
//! member names, read at some name offset, with each member's class pointer
//! leading to a structure that names the expected property class.

use crate::error::ScanError;
use crate::limits::{MAX_CHAIN_STEPS, MAX_FFIELD_OFFSET, MAX_REFLECTION_OFFSET};

use super::{Env, Prop, System};

/// Member names and property classes of the core structs. Every one is a
/// plain C++ struct of the engine.
const EXPECTED: [(&str, &[&str], &[&str]); 7] = [
    ("guid", &["A", "B", "C", "D"], &["IntProperty"]),
    ("color", &["B", "G", "R", "A"], &["ByteProperty"]),
    ("linearcolor", &["R", "G", "B", "A"], &["FloatProperty"]),
    (
        "vector",
        &["X", "Y", "Z"],
        &["FloatProperty", "DoubleProperty"],
    ),
    (
        "rotator",
        &["Pitch", "Yaw", "Roll"],
        &["FloatProperty", "DoubleProperty"],
    ),
    (
        "quat",
        &["X", "Y", "Z", "W"],
        &["FloatProperty", "DoubleProperty"],
    ),
    (
        "transform",
        &["Rotation", "Translation", "Scale3D"],
        &["StructProperty"],
    ),
];

/// Smallest number of core structs that must agree.
const MIN_AGREEING_STRUCTS: usize = 3;

/// Most structs with a parent examined when telling a member list from a
/// link chain.
const MAX_DERIVED_SAMPLES: usize = 2000;

/// The offsets of the modern property system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ModernOffsets {
    pub child_properties: u32,
    pub class: u32,
    pub next: u32,
    pub name: u32,
    pub fc_name: u32,
}

fn offsets_8(from: u32, to: u32) -> impl Iterator<Item = u32> {
    (from.next_multiple_of(8)..to).step_by(8)
}

fn offsets_4(from: u32, to: u32) -> impl Iterator<Item = u32> {
    (from..to).step_by(4)
}

/// The core structs present in the image, with their expected members.
fn present(env: &Env<'_, '_>) -> Vec<(u64, &'static [&'static str], &'static [&'static str])> {
    EXPECTED
        .iter()
        .filter_map(|&(name, members, classes)| {
            Some((*env.anchors.structs.get(name)?, members, classes))
        })
        .collect()
}

/// Follows `next` from `head`, collecting at most `limit` members.
fn walk(env: &Env<'_, '_>, head: u64, next: u32, limit: usize) -> Vec<u64> {
    let mut out = Vec::new();
    let mut current = head;
    while current != 0 && out.len() < limit {
        out.push(current);
        match env.ctx.u64(current + u64::from(next)) {
            Some(link) => current = link,
            None => break,
        }
    }
    out
}

fn names_match(names: &[Option<String>], expected: &[&str]) -> bool {
    let matches = |order: &mut dyn Iterator<Item = &&str>| {
        names.len() == expected.len()
            && names
                .iter()
                .zip(order)
                .all(|(got, want)| got.as_deref() == Some(*want))
    };
    matches(&mut expected.iter()) || matches(&mut expected.iter().rev())
}

/// A chain of members and the names read from them.
struct Candidate {
    child_properties: u32,
    next: u32,
    name: u32,
    agreeing: usize,
    structs: Vec<(usize, Vec<u64>)>,
}

/// Structs that have a parent, as `(struct, parent)`.
fn derived_structs(env: &Env<'_, '_>) -> Vec<(u64, u64)> {
    env.universe
        .objs
        .iter()
        .filter(|o| o.class == env.anchors.class || o.class == env.anchors.script_struct)
        .filter_map(|o| {
            let parent = env
                .ctx
                .u64(o.addr + u64::from(env.fields.ustruct.super_struct))?;
            (parent != 0).then_some((o.addr, parent))
        })
        .take(MAX_DERIVED_SAMPLES)
        .collect()
}

/// How many structs have a list through `candidate` that runs on into the
/// list of their parent.
///
/// The child properties of a struct are its own and end with the last of
/// them. `PropertyLink` and `RefLink` also lead through the members of its
/// parents, so a list that reaches the head of the parent's list is a link
/// chain and not the child properties.
fn leaks_into_parent(env: &Env<'_, '_>, derived: &[(u64, u64)], candidate: &Candidate) -> usize {
    let head_of = |owner: u64| {
        env.ctx
            .u64(owner + u64::from(candidate.child_properties))
            .filter(|&head| head != 0)
    };
    derived
        .iter()
        .filter(|&&(owner, parent)| {
            let (Some(head), Some(parent_head)) = (head_of(owner), head_of(parent)) else {
                return false;
            };
            walk(env, head, candidate.next, MAX_CHAIN_STEPS).contains(&parent_head)
        })
        .count()
}

fn find_chain(env: &Env<'_, '_>, core: &[(u64, &[&str], &[&str])]) -> Result<Candidate, ScanError> {
    let ustruct = &env.fields.ustruct;
    let size = env.sizes.script_struct;
    let mut best: Vec<Candidate> = Vec::new();
    let mut best_score = 0usize;
    for child_properties in offsets_8(0x18, MAX_REFLECTION_OFFSET) {
        if child_properties == ustruct.super_struct
            || child_properties == ustruct.children
            || child_properties + 8 > size
        {
            continue;
        }
        let heads: Vec<(usize, u64)> = core
            .iter()
            .enumerate()
            .filter_map(|(i, &(addr, _, _))| {
                let head = env.ctx.u64(addr + u64::from(child_properties))?;
                let plain =
                    head != 0 && env.universe.get(head).is_none() && env.ctx.is_pointer(head);
                plain.then_some((i, head))
            })
            .collect();
        if heads.len() < MIN_AGREEING_STRUCTS {
            continue;
        }
        for next in offsets_8(8, MAX_FFIELD_OFFSET) {
            let chains: Vec<(usize, Vec<u64>)> = heads
                .iter()
                .map(|&(i, head)| {
                    let limit = core.get(i).map_or(0, |c| c.1.len());
                    (i, walk(env, head, next, limit))
                })
                .collect();
            for name in offsets_4(8, MAX_FFIELD_OFFSET) {
                let agreeing: Vec<(usize, Vec<u64>)> = chains
                    .iter()
                    .filter(|(i, chain)| {
                        let names: Vec<Option<String>> = chain
                            .iter()
                            .map(|&member| env.fname_at(member + u64::from(name)))
                            .collect();
                        core.get(*i).is_some_and(|c| names_match(&names, c.1))
                    })
                    .cloned()
                    .collect();
                if agreeing.len() < best_score {
                    continue;
                }
                let candidate = Candidate {
                    child_properties,
                    next,
                    name,
                    agreeing: agreeing.len(),
                    structs: agreeing,
                };
                if candidate.agreeing > best_score {
                    best_score = candidate.agreeing;
                    best.clear();
                }
                best.push(candidate);
            }
        }
    }
    let needed = MIN_AGREEING_STRUCTS.max((core.len() * 6).div_ceil(10));
    if best_score < needed.min(core.len()) || best_score < MIN_AGREEING_STRUCTS {
        return Err(ScanError::missing(
            "ustruct.child_properties",
            "no pointer in the core structs leads to a chain of non-object members with the expected names",
        ));
    }
    // Several chains can satisfy the names. A struct also lists its members
    // through `PropertyLink`, whose head is the first member and whose
    // members' own `Next` leads on the same way, and the editor build keeps
    // both. For a struct without a parent the lists are identical, so the
    // first step looks at structs that have one: only the link chains run on
    // into the parent's members. What remains is told apart by where the
    // engine puts the fields: the child properties follow `Children`, and a
    // member's name follows its `Next`.
    if best.len() > 1 {
        let derived = derived_structs(env);
        let leaks: Vec<usize> = best
            .iter()
            .map(|c| leaks_into_parent(env, &derived, c))
            .collect();
        if let Some(&fewest) = leaks.iter().min() {
            best = best
                .into_iter()
                .zip(leaks)
                .filter_map(|(candidate, count)| (count == fewest).then_some(candidate))
                .collect();
        }
    }
    if best.len() > 1 {
        let after_children = ustruct.children + 8;
        let adjacent: Vec<usize> = best
            .iter()
            .enumerate()
            .filter(|(_, c)| c.child_properties == after_children && c.name == c.next + 8)
            .map(|(i, _)| i)
            .collect();
        if let [only] = adjacent.as_slice() {
            best = best.drain(*only..=*only).collect();
        }
    }
    match best.len() {
        1 => best
            .pop()
            .ok_or_else(|| ScanError::missing("ustruct.child_properties", "empty")),
        _ => Err(ScanError::Ambiguous {
            field: "ustruct.child_properties",
            candidates: best
                .iter()
                .map(|c| {
                    format!(
                        "child {:#x} next {:#x} name {:#x}",
                        c.child_properties, c.next, c.name
                    )
                })
                .collect::<Vec<_>>()
                .join("; "),
        }),
    }
}

/// Finds the class pointer of a member and the name offset inside the
/// `FFieldClass` it points to.
fn find_class(
    env: &Env<'_, '_>,
    core: &[(u64, &[&str], &[&str])],
    candidate: &Candidate,
) -> Result<(u32, u32, usize, usize), ScanError> {
    let members: Vec<(u64, &[&str])> = candidate
        .structs
        .iter()
        .flat_map(|(i, chain)| {
            let classes = core.get(*i).map_or(&[][..], |c| c.2);
            chain.iter().map(move |&m| (m, classes))
        })
        .collect();
    let mut best: Vec<(u32, u32, usize)> = Vec::new();
    let mut best_hits = 0usize;
    for class in offsets_8(8, 0x40) {
        if class == candidate.next {
            continue;
        }
        let pointers: Vec<(u64, &[&str])> = members
            .iter()
            .filter_map(|&(m, classes)| Some((env.ctx.u64(m + u64::from(class))?, classes)))
            .filter(|&(p, _)| p != 0 && env.ctx.is_pointer(p))
            .collect();
        for fc_name in offsets_4(0, MAX_FFIELD_OFFSET) {
            let hits = pointers
                .iter()
                .filter(|&&(p, classes)| {
                    env.fname_at(p + u64::from(fc_name))
                        .is_some_and(|n| classes.contains(&n.as_str()))
                })
                .count();
            if hits < best_hits {
                continue;
            }
            if hits > best_hits {
                best_hits = hits;
                best.clear();
            }
            best.push((class, fc_name, hits));
        }
    }
    if best_hits * 10 < members.len() * 9 || best_hits == 0 {
        return Err(ScanError::missing(
            "ffield.class",
            "no member offset leads to a structure that names the expected property class",
        ));
    }
    match best.as_slice() {
        [(class, fc_name, hits)] => Ok((*class, *fc_name, *hits, members.len())),
        many => Err(ScanError::Ambiguous {
            field: "ffield.class",
            candidates: many
                .iter()
                .map(|(c, n, _)| format!("class {c:#x} name {n:#x}"))
                .collect::<Vec<_>>()
                .join("; "),
        }),
    }
}

/// Whether the `Children` list of `Guid` yields the member names as objects.
fn legacy_members_found(env: &Env<'_, '_>, core: &[(u64, &[&str], &[&str])]) -> usize {
    core.iter()
        .filter(|&&(addr, names, _)| {
            let members = System::Legacy.chain(env, addr);
            let got: Vec<Option<String>> = members
                .iter()
                .take(names.len())
                .map(|&m| System::Legacy.name(env, m))
                .collect();
            members.len() >= names.len() && names_match(&got, names)
        })
        .count()
}

/// Decides the property system and, for the modern one, its offsets.
pub(crate) fn detect(env: &Env<'_, '_>) -> Result<System, ScanError> {
    let core = present(env);
    if core.len() < MIN_AGREEING_STRUCTS {
        return Err(ScanError::missing(
            "ustruct.child_properties",
            format!("only {} of the core structs are present", core.len()),
        ));
    }
    let ctx = env.ctx;
    let modern = find_chain(env, &core);
    let candidate = match modern {
        Ok(candidate) => candidate,
        Err(modern_error @ ScanError::MissingEvidence { .. }) => {
            // Not a modern image. A legacy one lists the members as objects.
            let legacy = legacy_members_found(env, &core);
            if legacy >= MIN_AGREEING_STRUCTS.min(core.len()) {
                if let Some(given) = ctx.options.overrides.ustruct_child_properties {
                    return Err(ScanError::OverrideContradiction {
                        field: "ustruct.child_properties",
                        given: u64::from(given),
                        found: 0,
                    });
                }
                ctx.evidence(
                    "ustruct.child_properties",
                    0,
                    format!(
                        "absent: no non-object member chain, and the Children list yields the \
                         member names as objects for {legacy} of {} core structs",
                        core.len()
                    ),
                    core.len(),
                    legacy,
                );
                return Ok(System::Legacy);
            }
            return Err(modern_error);
        }
        Err(other) => return Err(other),
    };
    let (class, fc_name, hits, total) = find_class(env, &core, &candidate)?;
    let child_properties = ctx.resolve(
        "ustruct.child_properties",
        Some(candidate.child_properties),
        String::new,
    )?;
    let next = ctx.resolve("ffield.next", Some(candidate.next), String::new)?;
    let name = ctx.resolve("ffield.name", Some(candidate.name), String::new)?;
    let class = ctx.resolve("ffield.class", Some(class), String::new)?;
    let fc_name = ctx.resolve("field_class.name", Some(fc_name), String::new)?;
    let chain_method = format!(
        "chain of non-object members yields the known member names for {} of {} core structs",
        candidate.agreeing,
        core.len()
    );
    ctx.evidence(
        "ustruct.child_properties",
        u64::from(child_properties),
        chain_method.clone(),
        core.len(),
        candidate.agreeing,
    );
    ctx.evidence(
        "ffield.next",
        u64::from(next),
        chain_method.clone(),
        core.len(),
        candidate.agreeing,
    );
    ctx.evidence(
        "ffield.name",
        u64::from(name),
        chain_method,
        core.len(),
        candidate.agreeing,
    );
    let class_method =
        "member class pointer leads to a structure naming the expected property class";
    ctx.evidence("ffield.class", u64::from(class), class_method, total, hits);
    ctx.evidence(
        "field_class.name",
        u64::from(fc_name),
        class_method,
        total,
        hits,
    );
    Ok(System::Modern(ModernOffsets {
        child_properties,
        class,
        next,
        name,
        fc_name,
    }))
}

/// `FField::FlagsPrivate`: a non-zero 32-bit field between the name and the
/// property members that holds few distinct values, all of them object flags.
pub(crate) fn solve_ffield_flags(
    env: &Env<'_, '_>,
    m: &ModernOffsets,
    props: &[Prop],
    core: &super::core::Core,
    _size: u32,
) -> Result<Option<u32>, ScanError> {
    let ctx = env.ctx;
    let name_fields: Vec<u32> = [
        Some(env.fname.comparison_index),
        env.fname.display_index,
        env.fname.number,
    ]
    .into_iter()
    .flatten()
    .map(|o| m.name + o)
    .collect();
    let mut found = Vec::new();
    for offset in offsets_4(m.name + 4, core.array_dim) {
        if name_fields.contains(&offset) || offset == m.next || offset == m.next + 4 {
            continue;
        }
        let values: Vec<u32> = props
            .iter()
            .filter_map(|p| ctx.u32(p.addr + u64::from(offset)))
            .collect();
        let mut distinct = values.clone();
        distinct.sort_unstable();
        distinct.dedup();
        let sane = values.len() >= 8
            && values.iter().all(|&v| v != 0 && v < 0x1000_0000)
            && distinct.len() <= 16;
        if sane {
            found.push(offset);
        }
    }
    let solved = match found.as_slice() {
        [only] => Some(*only),
        _ => None,
    };
    let flags = ctx.resolve_optional("ffield.flags", solved)?;
    match (flags, solved) {
        (Some(offset), Some(_)) => ctx.evidence(
            "ffield.flags",
            u64::from(offset),
            "non-zero 32-bit field after the name with few distinct values, all below 0x10000000",
            props.len(),
            props.len(),
        ),
        (None, _) => ctx.warn(format!(
            "ffield.flags: {} offsets fit the object-flag pattern; none is chosen",
            found.len()
        )),
        _ => {}
    }
    Ok(flags)
}

/// `FFieldClass::CastFlags`: a 64-bit field where every class has all the
/// bits of its super class and, for most, at least one more.
pub(crate) fn solve_field_class_cast_flags(
    env: &Env<'_, '_>,
    m: &ModernOffsets,
    props: &[Prop],
) -> Result<Option<u32>, ScanError> {
    let ctx = env.ctx;
    let mut classes: Vec<u64> = props
        .iter()
        .filter_map(|p| ctx.u64(p.addr + u64::from(m.class)))
        .collect();
    classes.sort_unstable();
    classes.dedup();
    let name_span = m.fc_name..m.fc_name + env.fname.size;
    let is_class_like = |addr: u64| -> bool {
        addr != 0
            && env
                .fname_at(addr + u64::from(m.fc_name))
                .is_some_and(|n| n.contains("Property"))
    };
    // The super class pointer: null or another class-like structure.
    let super_offset: Vec<u32> = offsets_8(0, 0x60)
        .filter(|o| !name_span.contains(o))
        .filter(|&o| {
            let values: Vec<u64> = classes
                .iter()
                .filter_map(|&c| ctx.u64(c + u64::from(o)))
                .collect();
            let non_null = values.iter().filter(|&&v| v != 0).count();
            non_null * 2 >= values.len()
                && values.iter().all(|&v| v == 0 || is_class_like(v))
                && non_null > 0
        })
        .collect();
    let solved = match super_offset.as_slice() {
        [s] => {
            let pairs: Vec<(u64, u64)> = classes
                .iter()
                .filter_map(|&c| Some((c, ctx.u64(c + u64::from(*s))?)))
                .filter(|&(_, parent)| parent != 0)
                .collect();
            let mut found = Vec::new();
            for offset in offsets_8(0, 0x60) {
                if offset == *s || name_span.contains(&offset) {
                    continue;
                }
                let read = |addr: u64| ctx.u64(addr + u64::from(offset));
                let fits = pairs
                    .iter()
                    .all(|&(child, parent)| match (read(child), read(parent)) {
                        (Some(c), Some(p)) => c != 0 && c & p == p,
                        _ => false,
                    });
                let strict = pairs
                    .iter()
                    .filter(|&&(child, parent)| read(child) != read(parent))
                    .count();
                if pairs.len() >= 5 && fits && strict * 10 >= pairs.len() * 6 {
                    found.push(offset);
                }
            }
            match found.as_slice() {
                [only] => Some(*only),
                _ => None,
            }
        }
        _ => None,
    };
    let cast = ctx.resolve_optional("field_class.cast_flags", solved)?;
    match (cast, solved) {
        (Some(offset), Some(_)) => ctx.evidence(
            "field_class.cast_flags",
            u64::from(offset),
            "every class has all cast bits of its super class and most add one",
            classes.len(),
            classes.len(),
        ),
        (None, _) => ctx.warn("field_class.cast_flags: no unique 64-bit field nests the cast bits of the class hierarchy"),
        _ => {}
    }
    Ok(cast)
}
