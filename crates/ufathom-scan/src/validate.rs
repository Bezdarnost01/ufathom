//! Step 7: global invariants, checked on the finished layout with the typed
//! views of `ufathom-engine`.
//!
//! - every sampled property fits inside its struct and does not overlap
//!   another member,
//! - every class chain ends at `Object` and every struct chain ends at null,
//!   without a cycle,
//! - a derived struct is at least as large as its base,
//! - the name table loads without unreadable blocks.
//!
//! A violation by more than [`MAX_VIOLATION_SHARE`] of the checked items
//! fails discovery with the invariant and the counts. A few violations are
//! damage to the target, not a wrong layout: they become warnings.

use ufathom_engine::{
    LoadOptions, NameTable, ObjectArrayView, for_layout, read_struct, super_chain, validate_members,
};
use ufathom_model::EngineLayout;

use crate::ctx::Ctx;
use crate::error::ScanError;
use crate::limits::{MAX_VALIDATION_STRUCTS, MAX_VIOLATION_SHARE, MIN_VIOLATIONS_TO_FAIL};
use crate::universe::{Anchors, Obj, Universe};

/// A tally of one invariant.
#[derive(Debug, Default)]
struct Tally {
    checked: usize,
    violations: usize,
    example: Option<String>,
}

impl Tally {
    fn pass(&mut self) {
        self.checked += 1;
    }

    fn fail(&mut self, example: impl FnOnce() -> String) {
        self.checked += 1;
        self.violate(example);
    }

    /// A violation of an item already counted as checked.
    fn violate(&mut self, example: impl FnOnce() -> String) {
        self.violations += 1;
        if self.example.is_none() {
            self.example = Some(example());
        }
    }

    /// Turns the tally into an error or a warning.
    fn conclude(self, ctx: &Ctx<'_>, invariant: &'static str) -> Result<(), ScanError> {
        if self.violations == 0 {
            return Ok(());
        }
        let example = self.example.unwrap_or_default();
        if self.checked == 0
            || (self.violations as f64 > MAX_VIOLATION_SHARE * self.checked as f64
                && self.violations >= MIN_VIOLATIONS_TO_FAIL)
        {
            return Err(ScanError::ValidationFailed {
                invariant,
                checked: self.checked,
                violations: self.violations,
                example,
            });
        }
        ctx.warn(format!(
            "{invariant}: {} of {} checked items violate it ({example}); the target looks damaged, the layout does not",
            self.violations, self.checked
        ));
        Ok(())
    }
}

/// Checks the invariants. `layout` is the finished layout.
pub(crate) fn validate(
    ctx: &Ctx<'_>,
    layout: &EngineLayout,
    universe: &Universe,
    anchors: &Anchors,
    structs: &[Obj],
) -> Result<(), ScanError> {
    let mem = ctx.reader();
    let table = NameTable::load(
        mem,
        &layout.names,
        &LoadOptions::for_layout(layout, ctx.options.pool),
    )?;
    let stats = table.stats();
    if !stats.unreadable_blocks.is_empty() || !stats.damaged_blocks.is_empty() {
        ctx.warn(format!(
            "name table: {} blocks unreadable and {} damaged; names stored there do not resolve",
            stats.unreadable_blocks.len(),
            stats.damaged_blocks.len()
        ));
    }
    let view = ObjectArrayView::new(mem, &layout.objects)?;
    ctx.evidence(
        "validation.objects",
        u64::from(view.num()),
        "the object array loads with the solved layout",
        1,
        1,
    );

    let sample: Vec<&Obj> = structs.iter().take(MAX_VALIDATION_STRUCTS).collect();
    let mut chains = Tally::default();
    let mut sizes = Tally::default();
    for obj in &sample {
        let mut last = None;
        let mut broken: Option<String> = None;
        for link in super_chain(mem, &layout.ustruct, obj.addr) {
            match link {
                Ok(addr) => last = Some(addr),
                Err(error) => {
                    broken = Some(error.to_string());
                    break;
                }
            }
        }
        let name = || universe.name(&table, obj).unwrap_or("<unnamed>").to_owned();
        // Classes end at Object. A script struct has no `UObject` ancestor in
        // its chain: it ends at null, possibly through other script structs.
        let is_class = obj.class == anchors.class;
        match (broken, last) {
            (Some(error), _) => chains.fail(|| format!("{}: {error}", name())),
            (None, Some(top)) if top == anchors.object || !is_class => chains.pass(),
            (None, top) => chains.fail(|| {
                format!(
                    "{}: chain ends at {:#x}, not at Object",
                    name(),
                    top.unwrap_or(0)
                )
            }),
        }
        // Derived size is at least the base size.
        let Ok(view) = read_struct(mem, &layout.ustruct, obj.addr) else {
            continue;
        };
        if view.super_struct == 0 {
            continue;
        }
        if let Ok(base) = read_struct(mem, &layout.ustruct, view.super_struct) {
            if view.properties_size >= base.properties_size {
                sizes.pass();
            } else {
                sizes.fail(|| {
                    format!(
                        "{} is {:#x}, its base {:#x}",
                        name(),
                        view.properties_size,
                        base.properties_size
                    )
                });
            }
        }
    }
    chains.conclude(
        ctx,
        "super chains end at Object (classes) or at null (script structs)",
    )?;
    sizes.conclude(ctx, "a derived struct is at least as large as its base")?;

    let reader = for_layout(mem, layout, &table);
    let mut fits = Tally::default();
    let functions = universe
        .objs
        .iter()
        .filter(|o| o.class == anchors.function)
        .take(MAX_VALIDATION_STRUCTS);
    for owner in sample.iter().copied().chain(functions) {
        let Ok(view) = read_struct(mem, &layout.ustruct, owner.addr) else {
            continue;
        };
        let mut members = Vec::new();
        for item in reader.fields(owner.addr) {
            if let Ok(raw) = item.and_then(|field| reader.decode(field)) {
                members.push(raw);
            }
        }
        for _ in &members {
            fits.pass();
        }
        for problem in validate_members(view.properties_size, &members) {
            fits.violate(|| {
                format!(
                    "{}: {problem}",
                    universe.name(&table, owner).unwrap_or("<unnamed>")
                )
            });
        }
    }
    fits.conclude(
        ctx,
        "every property fits inside its struct and overlaps no other member",
    )?;
    Ok(())
}
