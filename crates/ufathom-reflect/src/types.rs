//! Phases 3 and 4: paths of types, then the types themselves.

use std::collections::HashMap;

use ufathom_engine::{NameTable, object_path};
use ufathom_mem::Addr;
use ufathom_model::{Diagnostic, Severity};

use crate::classify::{Classification, Kind};
use crate::ctx::{Ctx, Tally, describe, diagnostic};
use crate::error::ReflectError;
use crate::objects::ObjectTable;
use crate::parse::{Env, Item, Parsed, Reader, parse_type, still_current};

/// Objects whose paths one worker builds before handing back its results.
const PATH_BATCH: usize = 256;

/// Types one worker parses before handing back its results.
const PARSE_BATCH: usize = 32;

/// A type to read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TypeRef {
    /// Position in the object table.
    pub(crate) position: usize,
    pub(crate) kind: Kind,
}

/// Builds the path of every class, struct, enum and function.
///
/// An object whose path cannot be built (an unresolvable name, an unreadable
/// outer) is a failure and has no path: references to it later come out
/// unresolved.
pub(crate) fn build_paths(
    ctx: &Ctx<'_>,
    names: &NameTable,
    table: &ObjectTable,
    classes: &Classification,
    tally: &mut Tally,
) -> Result<(HashMap<Addr, String>, Vec<TypeRef>), ReflectError> {
    let mut wanted: Vec<TypeRef> = Vec::new();
    for (position, &kind) in classes.kinds.iter().enumerate() {
        if matches!(
            kind,
            Kind::Class | Kind::ScriptStruct | Kind::Enum | Kind::Function
        ) {
            wanted.push(TypeRef { position, kind });
        }
    }
    let layout = ctx.layout;
    let built = ctx.exec.map_chunks(&wanted, PATH_BATCH, |part| {
        let mut out = Vec::with_capacity(part.len());
        for item in part {
            if ctx.cancelled() {
                break;
            }
            let Some(entry) = table.entries.get(item.position) else {
                continue;
            };
            let path = object_path(
                ctx.cached,
                names,
                &layout.uobject,
                &layout.fname,
                entry.header.addr,
            );
            out.push((*item, path));
        }
        out
    });
    ctx.check()?;

    let mut paths: HashMap<Addr, String> = HashMap::with_capacity(wanted.len());
    let mut types: Vec<TypeRef> = Vec::new();
    for (item, path) in built.into_iter().flatten() {
        let Some(entry) = table.entries.get(item.position) else {
            continue;
        };
        match path {
            Ok(path) => {
                paths.insert(entry.header.addr, path);
                if item.kind != Kind::Function {
                    types.push(item);
                }
            }
            Err(error) => tally.failure(
                describe(entry.header.index, entry.header.addr),
                format!("the path of the object cannot be built: {error}"),
            ),
        }
    }
    Ok((paths, types))
}

/// What reading one type came to.
struct Outcome {
    item: Option<Item>,
    diagnostics: Vec<Diagnostic>,
    enum_uses: Vec<(String, u32)>,
    functions: u64,
    failures: u64,
    rereads: u64,
}

/// Reads every type and returns them with the diagnostics, in table order.
pub(crate) fn parse_all(
    ctx: &Ctx<'_>,
    env: &Env<'_>,
    cached: &Reader<'_>,
    raw: &Reader<'_>,
    types: &[TypeRef],
    tally: &mut Tally,
) -> Result<ParsedTypes, ReflectError> {
    let batches = ctx.exec.map_chunks(types, PARSE_BATCH, |part| {
        let mut outcomes = Vec::with_capacity(part.len());
        for item in part {
            if ctx.cancelled() {
                break;
            }
            outcomes.push(process(env, cached, raw, *item));
        }
        outcomes
    });
    ctx.check()?;

    let mut result = ParsedTypes::default();
    for outcome in batches.into_iter().flatten() {
        tally.diagnostics.extend(outcome.diagnostics);
        tally.failures += outcome.failures;
        tally.rereads += outcome.rereads;
        result.enum_uses.extend(outcome.enum_uses);
        result.functions += outcome.functions;
        match outcome.item {
            Some(Item::Struct(def)) => result.structs.push(def),
            Some(Item::Enum(def)) => result.enums.push(def),
            None => {}
        }
    }
    Ok(result)
}

/// The types read.
#[derive(Debug, Default)]
pub(crate) struct ParsedTypes {
    pub(crate) structs: Vec<ufathom_model::StructDef>,
    pub(crate) enums: Vec<ufathom_model::EnumDef>,
    pub(crate) enum_uses: Vec<(String, u32)>,
    pub(crate) functions: u64,
}

/// Reads one type and re-validates it.
///
/// The first read goes through the cache. If it fails, or the object no
/// longer sits in its slot afterwards, the type is read once more without the
/// cache. A second failure skips the type with a diagnostic.
fn process(env: &Env<'_>, cached: &Reader<'_>, raw: &Reader<'_>, item: TypeRef) -> Outcome {
    let Some(entry) = env.table.entries.get(item.position) else {
        return skipped(
            String::new(),
            "the object is no longer in the table".to_owned(),
            0,
        );
    };
    let Some(path) = env.paths.get(&entry.header.addr) else {
        return skipped(
            describe(entry.header.index, entry.header.addr),
            "the object has no path".to_owned(),
            0,
        );
    };
    let first = parse_type(env, cached, entry, item.kind, path);
    if let Ok(parsed) = first
        && still_current(env, entry)
    {
        return accepted(parsed, 0);
    }
    let second = parse_type(env, raw, entry, item.kind, path);
    match second {
        Ok(parsed) if still_current(env, entry) => accepted(parsed, 1),
        Ok(_) => skipped(
            path.clone(),
            format!(
                "the object changed while it was read: slot {} no longer holds it",
                entry.header.index
            ),
            1,
        ),
        Err(error) => skipped(path.clone(), format!("cannot be read: {error}"), 1),
    }
}

fn accepted(parsed: Parsed, rereads: u64) -> Outcome {
    let failures = u64::from(
        parsed
            .diagnostics
            .iter()
            .any(|problem| problem.severity == Severity::Error),
    );
    Outcome {
        item: Some(parsed.item),
        diagnostics: parsed.diagnostics,
        enum_uses: parsed.enum_uses,
        functions: parsed.functions,
        failures,
        rereads,
    }
}

fn skipped(path: String, message: String, rereads: u64) -> Outcome {
    Outcome {
        item: None,
        diagnostics: vec![diagnostic(&path, Severity::Error, message)],
        enum_uses: Vec::new(),
        functions: 0,
        failures: 1,
        rereads,
    }
}
