//! Members the engine does not reflect: the header of `UObject` and the core
//! of `UField`, `UStruct`, `UClass` and `UFunction`.
//!
//! A dump has no fields for these classes (their members are private C++
//! members), so without help they would be plain padding. The layout the dump
//! was read with states where each one is, and the generator turns those
//! offsets into fields. They go through the same planner as every other
//! field, so they get the same compile-time size and offset assertions, and a
//! layout that does not fit the class is degraded instead of trusted.

use ufathom_model::{EngineLayout, StructDef, StructKind};

use crate::types::{EntryKind, FNAME_ALIAS, RT, Resolved, TypeTable};

/// A member found in the layout.
#[derive(Debug, Clone)]
pub(crate) struct Member {
    /// Name, as the engine calls it.
    pub name: &'static str,
    /// Offset from the start of the object.
    pub offset: u32,
    /// Its Rust type.
    pub resolved: Resolved,
    /// What it is, for the doc comment.
    pub doc: &'static str,
}

fn value(rust: &str, size: u32) -> Resolved {
    Resolved {
        rust: rust.to_owned(),
        size,
        align: size,
        refs: Vec::new(),
    }
}

/// `Ptr<T>` to the generated class at `path`, or to the opaque object base
/// when it is not generated.
fn pointer(table: &TypeTable, path: &str) -> Resolved {
    match table.names.get(path).filter(|e| e.kind == EntryKind::Class) {
        Some(entry) => Resolved {
            rust: format!("{RT}::Ptr<{}>", entry.rust_path()),
            size: 8,
            align: 8,
            refs: vec![entry.module.clone()],
        },
        None => Resolved {
            rust: format!("{RT}::Ptr<{RT}::UObjectBase>"),
            size: 8,
            align: 8,
            refs: Vec::new(),
        },
    }
}

fn raw_pointer() -> Resolved {
    Resolved {
        rust: format!("{RT}::Ptr<u8>"),
        size: 8,
        align: 8,
        refs: Vec::new(),
    }
}

fn member(name: &'static str, offset: u32, resolved: Resolved, doc: &'static str) -> Member {
    Member {
        name,
        offset,
        resolved,
        doc,
    }
}

/// The members to add to `def`, if it is one of the engine's root classes
/// and the dump gives it no fields of its own.
pub(crate) fn members(def: &StructDef, layout: &EngineLayout, table: &TypeTable) -> Vec<Member> {
    if def.kind != StructKind::Class || !def.fields.is_empty() {
        return Vec::new();
    }
    let pkg = &def.package;
    let object = format!("{pkg}.Object");
    let field = format!("{pkg}.Field");
    let structure = format!("{pkg}.Struct");
    let class = format!("{pkg}.Class");
    let own = || pointer(table, &def.path);
    match (def.name.as_str(), def.super_path.as_deref()) {
        ("Object", None) => {
            let u = &layout.uobject;
            let name = Resolved {
                rust: FNAME_ALIAS.to_owned(),
                size: table.fname.size,
                align: table.fname.align,
                refs: Vec::new(),
            };
            vec![
                member("Vtable", 0, raw_pointer(), "virtual table pointer"),
                member("ObjectFlags", u.flags, value("u32", 4), "EObjectFlags"),
                member(
                    "InternalIndex",
                    u.index,
                    value("i32", 4),
                    "slot in the global object array",
                ),
                member(
                    "ClassPrivate",
                    u.class,
                    pointer(table, &class),
                    "the class of the object",
                ),
                member("NamePrivate", u.name, name, "the name of the object"),
                member("OuterPrivate", u.outer, own(), "the outer object"),
            ]
        }
        ("Field", Some(p)) if p == object => vec![member(
            "Next",
            layout.ufield.next,
            pointer(table, &field),
            "next field in the owner's member list",
        )],
        ("Struct", Some(p)) if p == field => {
            let s = &layout.ustruct;
            let mut out = vec![
                member("SuperStruct", s.super_struct, own(), "the parent type"),
                member(
                    "Children",
                    s.children,
                    pointer(table, &field),
                    "first UField member",
                ),
            ];
            if let Some(offset) = s.child_properties {
                out.push(member(
                    "ChildProperties",
                    offset,
                    raw_pointer(),
                    "first FField member",
                ));
            }
            out.push(member(
                "PropertiesSize",
                s.properties_size,
                value("i32", 4),
                "size of an instance",
            ));
            let (rust, size) = if s.min_alignment_width == 2 {
                ("u16", 2)
            } else {
                ("i32", 4)
            };
            out.push(member(
                "MinAlignment",
                s.min_alignment,
                value(rust, size),
                "alignment of an instance",
            ));
            out
        }
        ("Class", Some(p)) if p == structure => {
            let c = &layout.uclass;
            let mut out = Vec::new();
            if let Some(offset) = c.cast_flags {
                out.push(member(
                    "ClassCastFlags",
                    offset,
                    value("u64", 8),
                    "EClassCastFlags",
                ));
            }
            if let Some(offset) = c.default_object {
                out.push(member(
                    "ClassDefaultObject",
                    offset,
                    pointer(table, &object),
                    "the default object",
                ));
            }
            out
        }
        ("Function", Some(p)) if p == structure => {
            let f = &layout.ufunction;
            let mut out = vec![member(
                "FunctionFlags",
                f.flags,
                value("u32", 4),
                "EFunctionFlags",
            )];
            if let Some(offset) = f.native_func {
                out.push(member("Func", offset, raw_pointer(), "exec entry point"));
            }
            out
        }
        _ => Vec::new(),
    }
}
