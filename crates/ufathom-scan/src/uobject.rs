//! Step 3: the fields of `UObject` and the shape of `FName`.
//!
//! The index offset is known from step 1. The rest comes from a sample of
//! objects spread over the whole array:
//!
//! - **Name.** The 32-bit offsets where at least 99% of the sampled objects
//!   hold a value that is an entry of the name table. One such offset is a
//!   bare comparison index; two are a comparison and a display index. The
//!   number sits next to them and holds small values that are mostly zero.
//! - **Class.** The pointer offset whose chain of class links reaches a
//!   fixed point, and the fixed point is named `Class`.
//! - **Outer.** The pointer offset whose chains end at null, with a
//!   `Package` at the top.
//!
//! No field order is assumed: `Name` may come before `Class`.

use std::collections::{HashMap, HashSet};

use ufathom_engine::NameTable;
use ufathom_model::layout::{FNameLayout, NameTableLayout};

use crate::ctx::{Ctx, le_u32, le_u64};
use crate::error::ScanError;
use crate::limits::{
    MAX_CLASS_HOPS, MAX_OUTER_DEPTH, MAX_UOBJECT_OFFSET, MIN_NAME_AGREEMENT, OBJECT_PROBE_BYTES,
    OBJECT_SAMPLE_COUNT, TOLERATED_FAILURES,
};
use crate::names::{Names, load_table};

/// A sampled object and its first bytes.
#[derive(Debug, Clone)]
pub(crate) struct Sample {
    pub index: u32,
    pub addr: u64,
    pub bytes: Vec<u8>,
}

/// Reads names of objects through a solved `FName` layout.
pub(crate) struct NameReader<'t> {
    pub table: &'t NameTable,
    pub fname: FNameLayout,
    pub name_offset: u32,
}

impl NameReader<'_> {
    /// The name of the object at `addr`, with its instance number.
    pub(crate) fn name_at(&self, ctx: &Ctx<'_>, addr: u64) -> Option<String> {
        let bytes = ctx.read(
            addr.checked_add(u64::from(self.name_offset))?,
            self.fname.size as usize,
        )?;
        self.table.resolve(&self.fname, &bytes).ok()
    }
}

/// The solved fields and the loaded name table.
pub(crate) struct ObjectFields {
    pub names: NameTableLayout,
    pub table: NameTable,
    pub fname: FNameLayout,
    pub name: u32,
    pub class: u32,
    pub outer: u32,
    /// Address of the `Class` class: the fixed point of the class links.
    pub metaclass: u64,
}

/// Evenly spaced objects over the whole array, plus the first few.
fn take_samples(ctx: &Ctx<'_>, slots: &[(u32, u64)]) -> Vec<Sample> {
    let step = (slots.len() / OBJECT_SAMPLE_COUNT).max(1);
    let picks = slots
        .iter()
        .step_by(step)
        .chain(slots.iter().take(32))
        .copied();
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (index, addr) in picks {
        if !seen.insert(addr) {
            continue;
        }
        let bytes = ctx
            .read(addr, OBJECT_PROBE_BYTES)
            .or_else(|| ctx.read(addr, 0x40));
        if let Some(bytes) = bytes {
            out.push(Sample { index, addr, bytes });
        }
    }
    out
}

/// Fewest distinct names a name field must show across the sample.
const MIN_DISTINCT_NAMES: usize = 8;

/// 4-aligned offsets of the object header that are searched.
fn offsets_4() -> impl Iterator<Item = u32> {
    (8..MAX_UOBJECT_OFFSET).step_by(4)
}

fn share(matched: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        matched as f64 / total as f64
    }
}

/// Offsets where a name index is stored, under each variant of the table.
struct NameScan {
    /// Offsets that decode, with the number of samples that decoded.
    offsets: Vec<(u32, usize)>,
    total: usize,
    /// The offset that decoded for the most samples, and how many, for the
    /// message when none reaches the bar.
    best: (u32, usize),
}

/// Whether `matched` of `total` samples is enough: at least 99%, or all but
/// a few damaged objects when the sample is large enough for that to mean
/// the offset is right.
fn agrees(matched: usize, total: usize) -> bool {
    let tolerated = (total / 50).min(TOLERATED_FAILURES);
    share(matched, total) >= MIN_NAME_AGREEMENT || total - matched <= tolerated
}

fn scan_names(
    names: &Names<'_, '_>,
    variant: Option<u32>,
    samples: &[Sample],
    index_offset: u32,
) -> NameScan {
    let mut offsets = Vec::new();
    let mut best = (0u32, 0usize);
    for offset in offsets_4() {
        if offset == index_offset {
            continue;
        }
        let values = column(samples, offset);
        let valid: Vec<u32> = values
            .iter()
            .copied()
            .filter(|&value| value != 0 && names.is_name(variant, value))
            .collect();
        let mut distinct = valid.clone();
        distinct.sort_unstable();
        distinct.dedup();
        // A field that holds one value in every object, such as the high half
        // of a pointer, decodes in a name array without being a name.
        let varied = distinct.len() >= MIN_DISTINCT_NAMES.min(samples.len() / 8);
        if varied && valid.len() > best.1 {
            best = (offset, valid.len());
        }
        if varied && agrees(valid.len(), samples.len()) {
            offsets.push((offset, valid.len()));
        }
    }
    NameScan {
        offsets,
        total: samples.len(),
        best,
    }
}

/// Reads one 32-bit field of every sample.
fn column(samples: &[Sample], offset: u32) -> Vec<u32> {
    samples
        .iter()
        .filter_map(|s| le_u32(&s.bytes, offset as usize))
        .collect()
}

/// Whether the values look like instance numbers: small, and not all zero.
fn looks_like_number(values: &[u32], samples: &[Sample]) -> bool {
    if values.is_empty() {
        return false;
    }
    let mut distinct = values.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < 2 {
        return false;
    }
    let small = values.iter().filter(|&&v| v <= 0xFFFF).count();
    let nonzero = values.iter().filter(|&&v| v != 0).count();
    let equals_index = values
        .iter()
        .zip(samples)
        .filter(|&(&v, s)| v == s.index)
        .count();
    share(small, values.len()) >= MIN_NAME_AGREEMENT
        && nonzero >= 2.max(values.len() / 100)
        && equals_index * 2 < values.len()
}

/// The names of two indices that differ, to tell the comparison one from the
/// display one: the comparison entry of a case-preserving build is the
/// lower-case twin of the display entry.
fn comparison_is_lower(table: &NameTable, a: &[u32], b: &[u32]) -> Option<bool> {
    let mut lower_a = 0usize;
    let mut lower_b = 0usize;
    for (&x, &y) in a.iter().zip(b) {
        if x == y {
            continue;
        }
        let (Some(tx), Some(ty)) = (table.resolve_index(x), table.resolve_index(y)) else {
            continue;
        };
        if tx != ty && tx.eq_ignore_ascii_case(ty) {
            if tx == tx.to_ascii_lowercase() {
                lower_a += 1;
            } else if ty == ty.to_ascii_lowercase() {
                lower_b += 1;
            }
        }
    }
    match (lower_a, lower_b) {
        (0, 0) => None,
        (x, y) => Some(x >= y),
    }
}

/// Decides the fields of `FName` from the offsets that decode.
fn solve_fname(
    ctx: &Ctx<'_>,
    table: &NameTable,
    decoding: &[(u32, usize)],
    samples: &[Sample],
    index_offset: u32,
) -> Result<(FNameLayout, u32), ScanError> {
    let mut offsets: Vec<u32> = decoding.iter().map(|&(offset, _)| offset).collect();
    offsets.sort_unstable();
    let (comparison, display) = match offsets.as_slice() {
        [only] => (*only, None),
        [a, b] => {
            let (ca, cb) = (column(samples, *a), column(samples, *b));
            let a_is_comparison = comparison_is_lower(table, &ca, &cb).unwrap_or(true);
            if a_is_comparison {
                (*a, Some(*b))
            } else {
                (*b, Some(*a))
            }
        }
        [] => {
            return Err(ScanError::missing(
                "uobject.name",
                "no 32-bit offset of the sampled objects decodes as a name index",
            ));
        }
        many => {
            return Err(ScanError::Ambiguous {
                field: "uobject.name",
                candidates: many
                    .iter()
                    .map(|o| format!("{o:#x}"))
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
    };
    let low = *offsets.first().unwrap_or(&comparison);
    let high = *offsets.last().unwrap_or(&comparison);
    let known = [Some(comparison), display];
    let mut number_candidates: Vec<u32> = Vec::new();
    for at in (low.saturating_sub(4)..=high + 4).step_by(4) {
        if at >= 8 && at != index_offset && !known.contains(&Some(at)) && !offsets.contains(&at) {
            number_candidates.push(at);
        }
    }
    // Prefer a field inside the span, then one after it, then one before.
    number_candidates.sort_by_key(|&at| {
        let rank = if at > low && at < high {
            0
        } else if at > high {
            1
        } else {
            2
        };
        (rank, at)
    });
    let number = number_candidates.into_iter().find(|&at| {
        let values = column(samples, at);
        looks_like_number(&values, samples)
    });
    let start = [Some(low), number]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(low);
    let end = [Some(high), number]
        .into_iter()
        .flatten()
        .max()
        .unwrap_or(high)
        + 4;
    let fname = FNameLayout {
        size: end - start,
        comparison_index: comparison - start,
        display_index: display.map(|d| d - start),
        number: number.map(|n| n - start),
    };
    let _ = ctx;
    Ok((fname, start))
}

/// Offsets where at least 98% of samples hold a pointer to a known object.
/// With `allow_null` a null counts too, as long as half the samples hold a
/// real object: the outer of a package is null.
fn object_pointer_offsets(
    samples: &[Sample],
    objects: &HashSet<u64>,
    allow_null: bool,
) -> Vec<u32> {
    let mut out = Vec::new();
    for offset in (8..MAX_UOBJECT_OFFSET).step_by(8) {
        let values: Vec<u64> = samples
            .iter()
            .filter_map(|s| le_u64(&s.bytes, offset as usize))
            .collect();
        let known = values.iter().filter(|v| objects.contains(v)).count();
        let null = values.iter().filter(|&&v| v == 0).count();
        let accepted = if allow_null { known + null } else { known };
        let enough_real = !allow_null || known * 2 >= values.len();
        if share(accepted, values.len()) >= 0.98 && enough_real {
            out.push(offset);
        }
    }
    out
}

/// Follows the class links from `start`. Returns the fixed point if the
/// chain reaches one.
fn class_fixed_point(ctx: &Ctx<'_>, start: u64, class_offset: u32) -> Option<u64> {
    let mut current = start;
    for _ in 0..MAX_CLASS_HOPS {
        let next = ctx.u64(current.checked_add(u64::from(class_offset))?)?;
        if next == current {
            return Some(current);
        }
        if next == 0 {
            return None;
        }
        current = next;
    }
    None
}

/// Solves the name table's last value, `FName` and the fields of `UObject`
/// that are not known after step 1.
pub(crate) fn solve_object_fields(
    ctx: &Ctx<'_>,
    names: &Names<'_, '_>,
    slots: &[(u32, u64)],
    index_offset: u32,
) -> Result<ObjectFields, ScanError> {
    let samples = take_samples(ctx, slots);
    if samples.len() < 16 {
        return Err(ScanError::missing(
            "uobject.name",
            format!("only {} sampled objects could be read", samples.len()),
        ));
    }

    // Name index offsets, per interpretation of the table.
    let mut scored: Vec<(Option<u32>, NameScan)> = Vec::new();
    for variant in names.variants() {
        scored.push((variant, scan_names(names, variant, &samples, index_offset)));
    }
    let best = scored
        .iter()
        .map(|(_, scan)| {
            (
                scan.offsets.len(),
                scan.offsets.iter().map(|&(_, n)| n).sum::<usize>(),
            )
        })
        .max()
        .unwrap_or((0, 0));
    if best.0 == 0 {
        let what = if names.is_pool() {
            "an index that lands on an entry start of the name pool under any block offset split"
        } else {
            "an index of the name array"
        };
        let (offset, matched) = scored
            .iter()
            .map(|(_, scan)| scan.best)
            .max_by_key(|&(_, matched)| matched)
            .unwrap_or((0, 0));
        let total = scored.first().map_or(0, |(_, scan)| scan.total);
        return Err(ScanError::missing(
            "uobject.name",
            format!(
                "no 32-bit offset of the sampled objects holds {what} for 99% of {total} samples; the best, offset {offset:#x}, did for {matched}"
            ),
        ));
    }
    let viable: Vec<(Option<u32>, &NameScan)> = scored
        .iter()
        .filter(|(_, scan)| {
            (
                scan.offsets.len(),
                scan.offsets.iter().map(|&(_, n)| n).sum::<usize>(),
            ) == best
        })
        .map(|(variant, scan)| (*variant, scan))
        .collect();
    let Some(&(_, first)) = viable.first() else {
        return Err(ScanError::missing(
            "uobject.name",
            "no variant decodes names",
        ));
    };
    let decoding = first.offsets.clone();
    let matched_names = decoding.iter().map(|&(_, n)| n).min().unwrap_or(0);
    let viable_variants: Vec<Option<u32>> = viable.iter().map(|&(v, _)| v).collect();
    let names_layout = names.describe(ctx, &viable_variants, first.total, matched_names)?;
    let table = load_table(
        ctx,
        &names_layout,
        &FNameLayout {
            size: 8,
            comparison_index: 0,
            display_index: None,
            number: Some(4),
        },
    )?;

    let (fname, name_offset) = solve_fname(ctx, &table, &decoding, &samples, index_offset)?;
    let table = if fname.number.is_none() {
        load_table(ctx, &names_layout, &fname)?
    } else {
        table
    };
    let reader = NameReader {
        table: &table,
        fname: fname.clone(),
        name_offset,
    };
    let method = "offset whose value is a name table entry for at least 99% of sampled objects";
    let name_offset = ctx.resolve("uobject.name", Some(name_offset), String::new)?;
    ctx.evidence(
        "uobject.name",
        u64::from(name_offset),
        method,
        first.total,
        matched_names,
    );
    ctx.evidence(
        "fname.comparison_index",
        u64::from(fname.comparison_index),
        "lower-case twin of the display entry when both differ, else the lower offset",
        first.total,
        matched_names,
    );
    if let Some(display) = fname.display_index {
        ctx.evidence(
            "fname.display_index",
            u64::from(display),
            method,
            first.total,
            matched_names,
        );
    }
    match fname.number {
        Some(number) => ctx.evidence(
            "fname.number",
            u64::from(number),
            "adjacent 32-bit field holding small values, mostly zero, never the index",
            first.total,
            first.total,
        ),
        None => ctx.warn("fname.number: no adjacent field holds instance numbers; the build keeps the number outside the FName"),
    }

    // Class and outer, from the pointer offsets.
    let object_set: HashSet<u64> = slots.iter().map(|&(_, addr)| addr).collect();
    let class_candidates = object_pointer_offsets(&samples, &object_set, false);
    let mut class_scores: Vec<(u32, usize, u64)> = Vec::new();
    for &offset in &class_candidates {
        let mut hits = 0usize;
        let mut meta: HashMap<u64, usize> = HashMap::new();
        for sample in &samples {
            if let Some(fixed) = class_fixed_point(ctx, sample.addr, offset)
                && reader
                    .name_at(ctx, fixed)
                    .is_some_and(|n| n.eq_ignore_ascii_case("Class"))
            {
                hits += 1;
                *meta.entry(fixed).or_default() += 1;
            }
        }
        let top = meta
            .into_iter()
            .max_by_key(|&(_, n)| n)
            .map_or(0, |(a, _)| a);
        class_scores.push((offset, hits, top));
    }
    class_scores.sort_by_key(|&(offset, hits, _)| (std::cmp::Reverse(hits), offset));
    let (class_solved, class_hits, metaclass) = match class_scores.as_slice() {
        [(offset, hits, meta), rest @ ..]
            if *hits * 10 >= samples.len() && rest.first().is_none_or(|r| r.1 < *hits) =>
        {
            (Some(*offset), *hits, *meta)
        }
        _ => (None, 0, 0),
    };
    let class = ctx.resolve("uobject.class", class_solved, || {
        "no pointer offset leads to a fixed point named Class".to_owned()
    })?;
    ctx.evidence(
        "uobject.class",
        u64::from(class),
        "class links reach a fixed point named Class",
        samples.len(),
        class_hits,
    );
    if class_hits * 10 < samples.len() * 9 {
        ctx.warn(format!(
            "uobject.class: {} of {} sampled objects have a class chain without a fixed point",
            samples.len() - class_hits,
            samples.len()
        ));
    }
    let metaclass = if metaclass == 0 {
        samples
            .iter()
            .find_map(|s| class_fixed_point(ctx, s.addr, class))
            .unwrap_or(0)
    } else {
        metaclass
    };

    let outer_candidates: Vec<u32> = object_pointer_offsets(&samples, &object_set, true)
        .into_iter()
        .filter(|&o| o != class)
        .collect();
    let mut outer_scores: Vec<(u32, usize)> = Vec::new();
    for &offset in &outer_candidates {
        let mut hits = 0usize;
        for sample in &samples {
            if outer_chain_ends_at_package(ctx, &reader, sample.addr, offset, class) {
                hits += 1;
            }
        }
        outer_scores.push((offset, hits));
    }
    outer_scores.sort_by_key(|&(offset, hits)| (std::cmp::Reverse(hits), offset));
    let outer_solved = match outer_scores.as_slice() {
        [(offset, hits), rest @ ..]
            if *hits * 10 >= samples.len() * 9 && rest.first().is_none_or(|r| r.1 < *hits) =>
        {
            Some(*offset)
        }
        _ => None,
    };
    let outer = ctx.resolve("uobject.outer", outer_solved, || {
        "no pointer offset gives outer chains that end at null with a Package".to_owned()
    })?;
    let outer_hits = outer_scores
        .iter()
        .find(|&&(o, _)| o == outer)
        .map_or(0, |&(_, h)| h);
    ctx.evidence(
        "uobject.outer",
        u64::from(outer),
        "outer chains end at null within a bounded depth with a Package at the top",
        samples.len(),
        outer_hits,
    );

    // Absolute offsets turn into the FName-relative layout.
    let fname = apply_fname_overrides(ctx, fname)?;
    Ok(ObjectFields {
        names: names_layout,
        table,
        fname,
        name: name_offset,
        class,
        outer,
        metaclass,
    })
}

fn apply_fname_overrides(ctx: &Ctx<'_>, fname: FNameLayout) -> Result<FNameLayout, ScanError> {
    let comparison = ctx.resolve(
        "fname.comparison_index",
        Some(fname.comparison_index),
        String::new,
    )?;
    let display = ctx.resolve_optional("fname.display_index", fname.display_index)?;
    let number = ctx.resolve_optional("fname.number", fname.number)?;
    Ok(FNameLayout {
        size: fname.size,
        comparison_index: comparison,
        display_index: display,
        number,
    })
}

/// Whether the outer chain of `start` ends at null within the depth bound
/// and its last object is a `Package`.
fn outer_chain_ends_at_package(
    ctx: &Ctx<'_>,
    reader: &NameReader<'_>,
    start: u64,
    outer_offset: u32,
    class_offset: u32,
) -> bool {
    let mut current = start;
    for _ in 0..MAX_OUTER_DEPTH {
        let Some(next) = ctx.u64(current.wrapping_add(u64::from(outer_offset))) else {
            return false;
        };
        if next == 0 {
            let Some(class) = ctx.u64(current.wrapping_add(u64::from(class_offset))) else {
                return false;
            };
            return reader
                .name_at(ctx, class)
                .is_some_and(|name| name.eq_ignore_ascii_case("Package"));
        }
        current = next;
    }
    false
}
