//! Phase 2: telling types from instances by walking class chains.

use std::collections::{HashMap, HashSet};

use ufathom_engine::{NameTable, object_path, super_chain};
use ufathom_mem::Addr;
use ufathom_model::flags::RF_CLASS_DEFAULT_OBJECT;

use crate::ctx::{Ctx, Tally, describe, u32_at};
use crate::error::ReflectError;
use crate::objects::ObjectTable;

/// Classes per worker.
const CLASS_BATCH: usize = 64;

/// The CoreUObject classes an object's class chain is compared against, in
/// the order of [`Metas`].
const META_NAMES: [&str; 5] = ["Class", "ScriptStruct", "Enum", "Function", "Package"];

/// Prefix of the package that holds [`META_NAMES`].
const CORE_PACKAGE: &str = "/Script/CoreUObject.";

/// What an object is, decided by its class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Its class derives from `Class`: a `UClass`, Blueprint generated classes
    /// included.
    Class,
    /// Its class derives from `ScriptStruct`.
    ScriptStruct,
    /// Its class derives from `Enum`.
    Enum,
    /// Its class derives from `Function`: delegate signatures included.
    Function,
    /// Its class derives from `Package`.
    Package,
    /// Anything else: class default objects, instances, properties.
    Other,
}

/// Addresses of the meta classes. Package is optional: nothing else depends
/// on it.
struct Metas {
    class: Addr,
    script_struct: Addr,
    enum_class: Addr,
    function: Addr,
    package: Option<Addr>,
}

impl Metas {
    fn kind_of(&self, class: Addr) -> Option<Kind> {
        if class == self.class {
            Some(Kind::Class)
        } else if class == self.script_struct {
            Some(Kind::ScriptStruct)
        } else if class == self.enum_class {
            Some(Kind::Enum)
        } else if class == self.function {
            Some(Kind::Function)
        } else if Some(class) == self.package {
            Some(Kind::Package)
        } else {
            None
        }
    }
}

/// The kind of every entry of the table, in table order.
pub(crate) struct Classification {
    pub(crate) kinds: Vec<Kind>,
}

/// Finds the meta classes: objects named like one of [`META_NAMES`] whose full
/// path is `/Script/CoreUObject.<name>`, compared without regard to case.
fn find_metas(
    ctx: &Ctx<'_>,
    names: &NameTable,
    table: &ObjectTable,
) -> Result<Metas, ReflectError> {
    // Name indices first, so only a handful of objects need a path.
    let mut by_index: HashMap<u32, usize> = HashMap::new();
    for (index, text) in names.iter() {
        if let Some(meta) = META_NAMES
            .iter()
            .position(|wanted| wanted.eq_ignore_ascii_case(text))
        {
            by_index.insert(index, meta);
        }
    }
    let layout = ctx.layout;
    let mut found: [Option<Addr>; META_NAMES.len()] = [None; META_NAMES.len()];
    for entry in &table.entries {
        let bytes = entry.header.name.as_slice();
        let numbered = layout
            .fname
            .number
            .and_then(|offset| u32_at(bytes, offset))
            .is_some_and(|number| number != 0);
        if numbered {
            continue;
        }
        let meta = layout
            .fname
            .display_index
            .and_then(|offset| u32_at(bytes, offset))
            .and_then(|index| by_index.get(&index))
            .or_else(|| {
                u32_at(bytes, layout.fname.comparison_index).and_then(|index| by_index.get(&index))
            });
        let Some(&meta) = meta else { continue };
        let Some(slot) = found.get_mut(meta) else {
            continue;
        };
        if slot.is_some() {
            continue;
        }
        let Some(name) = META_NAMES.get(meta) else {
            continue;
        };
        let wanted = format!("{CORE_PACKAGE}{name}");
        let confirmed = object_path(
            ctx.cached,
            names,
            &layout.uobject,
            &layout.fname,
            entry.header.addr,
        )
        .is_ok_and(|path| path.eq_ignore_ascii_case(&wanted));
        if confirmed {
            *slot = Some(entry.header.addr);
        }
    }
    let need = |position: usize| {
        found
            .get(position)
            .copied()
            .flatten()
            .ok_or(ReflectError::MissingCoreClass {
                name: META_NAMES.get(position).copied().unwrap_or("Class"),
            })
    };
    Ok(Metas {
        class: need(0)?,
        script_struct: need(1)?,
        enum_class: need(2)?,
        function: need(3)?,
        package: found.get(4).copied().flatten(),
    })
}

/// Classifies every object.
///
/// The class of each distinct class address is classified once: its super
/// chain is walked up to the first meta class. A class whose chain cannot be
/// walked (a cycle, an unreadable link) is reported once, as an error on that
/// class; its objects are treated as instances, since nothing says otherwise.
///
/// Every class an object names must itself be a `UClass`. One whose own class
/// does not reach `Class` is reported too.
pub(crate) fn classify(
    ctx: &Ctx<'_>,
    names: &NameTable,
    table: &ObjectTable,
    tally: &mut Tally,
) -> Result<(Classification, Vec<String>), ReflectError> {
    let metas = find_metas(ctx, names, table)?;
    ctx.check()?;

    let mut classes: Vec<Addr> = table
        .entries
        .iter()
        .map(|entry| entry.header.class)
        .collect();
    classes.sort_unstable();
    classes.dedup();

    let walked = ctx.exec.map_chunks(&classes, CLASS_BATCH, |part| {
        part.iter()
            .map(|&class| {
                if ctx.cancelled() {
                    return (class, Ok(Kind::Other));
                }
                (class, kind_of_class(ctx, &metas, class))
            })
            .collect::<Vec<_>>()
    });
    ctx.check()?;

    let mut kinds_by_class: HashMap<Addr, Kind> = HashMap::with_capacity(classes.len());
    let mut broken: Vec<(Addr, String)> = Vec::new();
    for (class, result) in walked.into_iter().flatten() {
        match result {
            Ok(kind) => {
                kinds_by_class.insert(class, kind);
            }
            Err(message) => {
                kinds_by_class.insert(class, Kind::Other);
                broken.push((class, message));
            }
        }
    }
    let chain_broken: HashSet<Addr> = broken.iter().map(|&(class, _)| class).collect();
    // Every class is a `UClass`: the class of the class object must be one.
    for &class in &classes {
        let Some(entry) = table.entry_at(class) else {
            continue;
        };
        let class_of_class = kinds_by_class.get(&entry.header.class);
        if class_of_class != Some(&Kind::Class)
            && !chain_broken.contains(&class)
            && !chain_broken.contains(&entry.header.class)
        {
            broken.push((
                class,
                format!(
                    "it is used as a class but its own class at {:#x} does not derive from Class",
                    entry.header.class
                ),
            ));
        }
    }
    broken.sort_by_key(|&(class, _)| class);
    for (class, message) in broken {
        let path = object_path(
            ctx.cached,
            names,
            &ctx.layout.uobject,
            &ctx.layout.fname,
            class,
        )
        .unwrap_or_else(|_| {
            table.entry_at(class).map_or_else(
                || format!("<class at {class:#x}>"),
                |entry| describe(entry.header.index, class),
            )
        });
        tally.failure(path, format!("class cannot be classified: {message}"));
    }

    // A class default object is an instance, even of `Class` or `ScriptStruct`
    // (`Default__Class` has the class `Class`). The flag tells them apart.
    let kinds: Vec<Kind> = table
        .entries
        .iter()
        .map(|entry| {
            if entry.header.flags & RF_CLASS_DEFAULT_OBJECT != 0 {
                return Kind::Other;
            }
            kinds_by_class
                .get(&entry.header.class)
                .copied()
                .unwrap_or(Kind::Other)
        })
        .collect();

    let mut packages = Vec::new();
    for (entry, kind) in table.entries.iter().zip(&kinds) {
        if *kind == Kind::Package
            && let Ok(name) = names.resolve(&ctx.layout.fname, entry.header.name.as_slice())
        {
            packages.push(name);
        }
    }
    Ok((Classification { kinds }, packages))
}

/// Walks the super chain of `class` up to the first meta class.
fn kind_of_class(ctx: &Ctx<'_>, metas: &Metas, class: Addr) -> Result<Kind, String> {
    for link in super_chain(ctx.cached, &ctx.layout.ustruct, class) {
        let addr = link.map_err(|error| error.to_string())?;
        if let Some(kind) = metas.kind_of(addr) {
            return Ok(kind);
        }
    }
    Ok(Kind::Other)
}
