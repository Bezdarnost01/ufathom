//! Mapping from dump property types to Rust types, with exact sizes.
//!
//! The sizes of the runtime views are computed here with the same rules
//! `#[repr(C)]` uses, and are checked against `ufathom-runtime` by a test of
//! this crate. A property whose size in the dump differs from the size of its
//! Rust type is degraded by the caller, never emitted with the wrong size.

use std::collections::BTreeMap;

use ufathom_model::PropertyType;
use ufathom_model::layout::FNameLayout;

use crate::report::DegradeReason;

/// The alias the generated crate gives `FName`.
pub(crate) const FNAME_ALIAS: &str = "crate::FName";

/// The runtime crate, imported as `rt` in every generated module.
pub(crate) const RT: &str = "rt";

/// Rounds `value` up to a multiple of `align`.
pub(crate) fn align_up(value: u32, align: u32) -> u32 {
    let align = align.max(1);
    value.div_ceil(align) * align
}

/// Size and alignment of a `#[repr(C)]` struct with these fields in order.
pub(crate) fn repr_c(fields: &[(u32, u32)]) -> (u32, u32) {
    let mut offset = 0u32;
    let mut align = 1u32;
    for &(size, field_align) in fields {
        offset = align_up(offset, field_align) + size;
        align = align.max(field_align);
    }
    (align_up(offset, align), align)
}

/// The `FName` type the generated crate uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FNameShape {
    /// The runtime type the alias points at.
    pub alias_target: String,
    /// Its size.
    pub size: u32,
    /// Its alignment.
    pub align: u32,
}

/// Picks the runtime `FName` that matches a layout exactly.
pub(crate) fn fname_shape(layout: &FNameLayout) -> FNameShape {
    let shape = |name: &str, size: u32, align: u32| FNameShape {
        alias_target: format!("::ufathom_runtime::{name}"),
        size,
        align,
    };
    match (
        layout.size,
        layout.comparison_index,
        layout.display_index,
        layout.number,
    ) {
        (4, 0, None, None) => shape("FName4", 4, 4),
        (8, 0, None, Some(4)) => shape("FName8", 8, 4),
        (12, 0, Some(4), Some(8)) => shape("FName12", 12, 4),
        (12, 0, Some(8), Some(4)) => shape("FName12Ue5", 12, 4),
        (size, ..) => FNameShape {
            alias_target: format!("::ufathom_runtime::FNameBytes<{size}>"),
            size,
            align: 1,
        },
    }
}

/// What a generated name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    /// A class.
    Class,
    /// A script struct.
    Struct,
    /// An enum.
    Enum,
}

/// A generated type that other types can refer to.
#[derive(Debug, Clone)]
pub(crate) struct NameEntry {
    /// Module the type lives in.
    pub module: String,
    /// Rust name.
    pub name: String,
    /// Class, struct or enum.
    pub kind: EntryKind,
}

impl NameEntry {
    /// Absolute path of the type inside the generated crate.
    pub(crate) fn rust_path(&self) -> String {
        format!("crate::{}::{}", self.module, self.name)
    }
}

/// A resolved Rust type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// The Rust type as source text.
    pub rust: String,
    /// Size of one value.
    pub size: u32,
    /// Alignment of the Rust type.
    pub align: u32,
    /// Modules of the generated crate the type mentions.
    pub refs: Vec<String>,
}

/// Why a type has no Rust type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unresolved {
    /// The kind of problem.
    pub reason: DegradeReason,
    /// Explanation.
    pub detail: String,
}

/// Everything needed to turn property types into Rust types.
#[derive(Debug)]
pub(crate) struct TypeTable {
    /// The `FName` alias.
    pub fname: FNameShape,
    /// Generated types by dump path. Only the first definition of a path.
    pub names: BTreeMap<String, NameEntry>,
    /// Size and alignment of the Rust type of every struct planned so far.
    pub layouts: BTreeMap<String, (u32, u32)>,
    /// Integer type of every enum: Rust name and size.
    pub enum_ints: BTreeMap<String, (&'static str, u32)>,
}

/// The integer type for an enum of `size` bytes.
pub(crate) fn enum_int(size: u32, signed: bool) -> (&'static str, u32) {
    match (size, signed) {
        (0..=1, false) => ("u8", 1),
        (0..=1, true) => ("i8", 1),
        (2, false) => ("u16", 2),
        (2, true) => ("i16", 2),
        (3..=4, false) => ("u32", 4),
        (3..=4, true) => ("i32", 4),
        (_, false) => ("u64", 8),
        (_, true) => ("i64", 8),
    }
}

impl TypeTable {
    fn leaf(&self, rust: &str, size: u32, align: u32) -> Resolved {
        Resolved {
            rust: rust.to_owned(),
            size,
            align,
            refs: Vec::new(),
        }
    }

    /// The class a pointer type points at: a generated class, or the opaque
    /// object base when the class is not in the dump.
    fn class_target(&self, path: &str) -> (String, Vec<String>) {
        match self.names.get(path) {
            Some(entry) if entry.kind == EntryKind::Class => {
                (entry.rust_path(), vec![entry.module.clone()])
            }
            _ => (format!("{RT}::UObjectBase"), Vec::new()),
        }
    }

    /// The Rust type for `ty`, with its size and alignment.
    ///
    /// # Errors
    ///
    /// [`Unresolved`] for a type with no exact Rust representation.
    pub(crate) fn resolve(&self, ty: &PropertyType) -> Result<Resolved, Unresolved> {
        let fname = (self.fname.size, self.fname.align);
        Ok(match ty {
            PropertyType::Int8 => self.leaf("i8", 1, 1),
            PropertyType::Int16 => self.leaf("i16", 2, 2),
            PropertyType::Int32 => self.leaf("i32", 4, 4),
            PropertyType::Int64 => self.leaf("i64", 8, 8),
            PropertyType::UInt8 => self.leaf("u8", 1, 1),
            PropertyType::UInt16 => self.leaf("u16", 2, 2),
            PropertyType::UInt32 => self.leaf("u32", 4, 4),
            PropertyType::UInt64 => self.leaf("u64", 8, 8),
            PropertyType::Float => self.leaf("f32", 4, 4),
            PropertyType::Double => self.leaf("f64", 8, 8),
            PropertyType::Bool { .. } => self.leaf(&format!("{RT}::UBool"), 1, 1),
            PropertyType::Name => self.leaf(FNAME_ALIAS, fname.0, fname.1),
            PropertyType::Str => self.leaf(&format!("{RT}::FString"), 16, 8),
            PropertyType::Text => self.leaf(&format!("{RT}::FText"), 0x18, 8),
            PropertyType::Object { class } => {
                let (target, refs) = self.class_target(class);
                Resolved {
                    rust: format!("{RT}::Ptr<{target}>"),
                    size: 8,
                    align: 8,
                    refs,
                }
            }
            PropertyType::Class { class, .. } => {
                let (target, refs) = self.class_target(class);
                Resolved {
                    rust: format!("{RT}::Ptr<{target}>"),
                    size: 8,
                    align: 8,
                    refs,
                }
            }
            PropertyType::WeakObject { .. } => self.leaf(&format!("{RT}::FWeakObjectPtr"), 8, 4),
            PropertyType::LazyObject { .. } => self.leaf(&format!("{RT}::FLazyObjectPtr"), 24, 4),
            PropertyType::SoftObject { .. } | PropertyType::SoftClass { .. } => {
                let path = repr_c(&[fname, (16, 8)]);
                let (size, align) = repr_c(&[(8, 4), (4, 4), path]);
                self.leaf(&format!("{RT}::FSoftObjectPtr<{FNAME_ALIAS}>"), size, align)
            }
            PropertyType::Interface { class } => {
                let (target, refs) = self.class_target(class);
                Resolved {
                    rust: format!("{RT}::FScriptInterface<{target}>"),
                    size: 16,
                    align: 8,
                    refs,
                }
            }
            PropertyType::Struct { path } => {
                let (size, align) = self.layouts.get(path).copied().ok_or_else(|| Unresolved {
                    reason: DegradeReason::UnknownType,
                    detail: format!("struct {path} is not in the dump or has a cyclic layout"),
                })?;
                let entry = self.names.get(path).ok_or_else(|| Unresolved {
                    reason: DegradeReason::UnknownType,
                    detail: format!("struct {path} has no generated name"),
                })?;
                Resolved {
                    rust: entry.rust_path(),
                    size,
                    align,
                    refs: vec![entry.module.clone()],
                }
            }
            PropertyType::Array { inner } => {
                let (inner, refs) = self.element(inner);
                Resolved {
                    rust: format!("{RT}::TArray<{inner}>"),
                    size: 16,
                    align: 8,
                    refs,
                }
            }
            PropertyType::Set { element } => {
                let (inner, refs) = self.element(element);
                Resolved {
                    rust: format!("{RT}::TSet<{inner}>"),
                    size: 0x50,
                    align: 8,
                    refs,
                }
            }
            PropertyType::Map { key, value } => {
                let (key, mut refs) = self.element(key);
                let (value, more) = self.element(value);
                refs.extend(more);
                Resolved {
                    rust: format!("{RT}::TMap<{key}, {value}>"),
                    size: 0x50,
                    align: 8,
                    refs,
                }
            }
            PropertyType::Enum { path, underlying } => match self.enum_type(path) {
                Some(found) => found,
                None => self.resolve(underlying)?,
            },
            PropertyType::Byte { path } => match self.enum_type(path) {
                Some(found) => found,
                None => self.leaf("u8", 1, 1),
            },
            PropertyType::Delegate { .. } => {
                let (size, align) = repr_c(&[(8, 4), fname]);
                self.leaf(
                    &format!("{RT}::FScriptDelegate<{FNAME_ALIAS}>"),
                    size,
                    align,
                )
            }
            PropertyType::MulticastInlineDelegate { .. } => self.leaf(
                &format!("{RT}::FMulticastInlineDelegate<{FNAME_ALIAS}>"),
                16,
                8,
            ),
            PropertyType::MulticastSparseDelegate { .. } => {
                self.leaf(&format!("{RT}::FMulticastSparseDelegate"), 1, 1)
            }
            PropertyType::FieldPath | PropertyType::Optional { .. } => {
                return Err(Unresolved {
                    reason: DegradeReason::NoRuntimeView,
                    detail: format!("the runtime has no view of {ty}"),
                });
            }
            PropertyType::Unknown { type_name } => {
                return Err(Unresolved {
                    reason: DegradeReason::UnknownType,
                    detail: format!("property class {type_name} is not decoded"),
                });
            }
        })
    }

    /// The type of an element of a container. The container only needs the
    /// name, so an element with no exact type is `()`.
    fn element(&self, ty: &PropertyType) -> (String, Vec<String>) {
        match self.resolve(ty) {
            Ok(r) => (r.rust, r.refs),
            Err(_) => ("()".to_owned(), Vec::new()),
        }
    }

    fn enum_type(&self, path: &str) -> Option<Resolved> {
        let entry = self.names.get(path).filter(|e| e.kind == EntryKind::Enum)?;
        let (_, size) = self.enum_ints.get(path).copied()?;
        Some(Resolved {
            rust: entry.rust_path(),
            size,
            align: size,
            refs: vec![entry.module.clone()],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(fname: FNameLayout) -> TypeTable {
        TypeTable {
            fname: fname_shape(&fname),
            names: BTreeMap::new(),
            layouts: BTreeMap::new(),
            enum_ints: BTreeMap::new(),
        }
    }

    fn fname(size: u32, display: Option<u32>, number: Option<u32>) -> FNameLayout {
        FNameLayout {
            size,
            comparison_index: 0,
            display_index: display,
            number,
        }
    }

    #[test]
    fn fname_shapes() {
        let target = |l: FNameLayout| fname_shape(&l).alias_target;
        assert!(target(fname(4, None, None)).ends_with("FName4"));
        assert!(target(fname(8, None, Some(4))).ends_with("FName8"));
        assert!(target(fname(12, Some(4), Some(8))).ends_with("FName12"));
        assert!(target(fname(12, Some(8), Some(4))).ends_with("FName12Ue5"));
        assert!(target(fname(16, Some(4), Some(8))).ends_with("FNameBytes<16>"));
        assert!(target(fname(8, Some(4), None)).ends_with("FNameBytes<8>"));
    }

    #[test]
    fn repr_c_matches_the_c_rules() {
        assert_eq!(repr_c(&[(8, 4), (12, 4)]), (20, 4));
        assert_eq!(
            repr_c(&[(8, 4), (4, 4), repr_c(&[(12, 4), (16, 8)])]),
            (48, 8)
        );
        assert_eq!(repr_c(&[(1, 1), (8, 8)]), (16, 8));
        assert_eq!(repr_c(&[]), (0, 1));
    }

    #[test]
    fn container_sizes_depend_on_the_name_shape() {
        let t8 = table(fname(8, None, Some(4)));
        let t12 = table(fname(12, Some(4), Some(8)));
        let soft = PropertyType::SoftObject {
            class: "/X.Y".into(),
        };
        assert_eq!(t8.resolve(&soft).map(|r| r.size), Ok(40));
        assert_eq!(t12.resolve(&soft).map(|r| r.size), Ok(48));
        let delegate = PropertyType::Delegate {
            signature: "/X.Y:Z".into(),
        };
        assert_eq!(t8.resolve(&delegate).map(|r| r.size), Ok(16));
        assert_eq!(t12.resolve(&delegate).map(|r| r.size), Ok(20));
    }

    #[test]
    fn unresolvable_types_name_a_reason() {
        let t = table(fname(8, None, Some(4)));
        let e = t.resolve(&PropertyType::FieldPath).unwrap_err();
        assert_eq!(e.reason, DegradeReason::NoRuntimeView);
        let e = t
            .resolve(&PropertyType::Struct {
                path: "/X.Missing".into(),
            })
            .unwrap_err();
        assert_eq!(e.reason, DegradeReason::UnknownType);
        // An unresolved element still gives a container.
        let arr = PropertyType::Array {
            inner: Box::new(PropertyType::Unknown {
                type_name: "Foo".into(),
            }),
        };
        assert_eq!(
            t.resolve(&arr).map(|r| r.rust),
            Ok("rt::TArray<()>".to_owned())
        );
    }

    /// The sizes this module assumes must be the sizes of the runtime types.
    fn check_runtime<N: ufathom_runtime::Pod>(layout: FNameLayout, name_size: usize) {
        use ufathom_runtime as rt;
        let t = table(layout);
        let size_align =
            |ty: &PropertyType| t.resolve(ty).map(|r| (r.size as usize, r.align as usize));
        let sa = |size: usize, align: usize| Ok((size, align));
        assert_eq!(size_of::<N>(), name_size);
        assert_eq!(
            size_align(&PropertyType::Name),
            sa(size_of::<N>(), align_of::<N>())
        );
        assert_eq!(
            size_align(&PropertyType::SoftObject {
                class: String::new()
            }),
            sa(
                size_of::<rt::FSoftObjectPtr<N>>(),
                align_of::<rt::FSoftObjectPtr<N>>()
            )
        );
        assert_eq!(
            size_align(&PropertyType::Delegate {
                signature: String::new()
            }),
            sa(
                size_of::<rt::FScriptDelegate<N>>(),
                align_of::<rt::FScriptDelegate<N>>()
            )
        );
        assert_eq!(
            size_align(&PropertyType::MulticastInlineDelegate {
                signature: String::new()
            }),
            sa(
                size_of::<rt::FMulticastInlineDelegate<N>>(),
                align_of::<rt::FMulticastInlineDelegate<N>>()
            )
        );
    }

    #[test]
    fn planner_sizes_match_the_runtime_types() {
        use ufathom_runtime as rt;
        check_runtime::<rt::FName4>(fname(4, None, None), 4);
        check_runtime::<rt::FName8>(fname(8, None, Some(4)), 8);
        check_runtime::<rt::FName12>(fname(12, Some(4), Some(8)), 12);
        check_runtime::<rt::FName12Ue5>(fname(12, Some(8), Some(4)), 12);
        check_runtime::<rt::FNameBytes<16>>(fname(16, Some(4), Some(8)), 16);
        let t = table(fname(8, None, Some(4)));
        let any = |ty: PropertyType| t.resolve(&ty).map(|r| (r.size as usize, r.align as usize));
        let sa = |size: usize, align: usize| Ok((size, align));
        assert_eq!(
            any(PropertyType::Str),
            sa(size_of::<rt::FString>(), align_of::<rt::FString>())
        );
        assert_eq!(
            any(PropertyType::Text),
            sa(size_of::<rt::FText>(), align_of::<rt::FText>())
        );
        assert_eq!(
            any(PropertyType::WeakObject {
                class: String::new()
            }),
            sa(
                size_of::<rt::FWeakObjectPtr>(),
                align_of::<rt::FWeakObjectPtr>()
            )
        );
        assert_eq!(
            any(PropertyType::LazyObject {
                class: String::new()
            }),
            sa(
                size_of::<rt::FLazyObjectPtr>(),
                align_of::<rt::FLazyObjectPtr>()
            )
        );
        assert_eq!(
            any(PropertyType::MulticastSparseDelegate {
                signature: String::new()
            }),
            sa(
                size_of::<rt::FMulticastSparseDelegate>(),
                align_of::<rt::FMulticastSparseDelegate>()
            )
        );
        assert_eq!(
            any(PropertyType::Interface {
                class: String::new()
            }),
            sa(
                size_of::<rt::FScriptInterface<u8>>(),
                align_of::<rt::FScriptInterface<u8>>()
            )
        );
        let int = PropertyType::Int32;
        let arr = PropertyType::Array {
            inner: Box::new(int.clone()),
        };
        assert_eq!(
            any(arr),
            sa(size_of::<rt::TArray<i32>>(), align_of::<rt::TArray<i32>>())
        );
        let set = PropertyType::Set {
            element: Box::new(int.clone()),
        };
        assert_eq!(
            any(set),
            sa(size_of::<rt::TSet<i32>>(), align_of::<rt::TSet<i32>>())
        );
        let map = PropertyType::Map {
            key: Box::new(int.clone()),
            value: Box::new(int),
        };
        assert_eq!(
            any(map),
            sa(
                size_of::<rt::TMap<i32, i32>>(),
                align_of::<rt::TMap<i32, i32>>()
            )
        );
        assert_eq!(
            any(PropertyType::Object {
                class: String::new()
            }),
            sa(size_of::<rt::Ptr<u8>>(), align_of::<rt::Ptr<u8>>())
        );
    }
}
