//! Comparison of classes and script structs.

use std::collections::BTreeMap;

use crate::dump::{FieldDef, StructDef};

use super::functions::diff_functions;
use super::pairing::{Pair, group_by, pair_up};
use super::report::{
    Ambiguity, AmbiguousSubject, DumpDiff, SlotChange, TypeChange, TypeDiff, TypeKey,
};

/// Types are identified by package and short name.
pub(super) fn struct_key(def: &StructDef) -> TypeKey {
    TypeKey::new(&def.package, &def.name)
}

/// Compares all structs of two dumps and records the result in `out`.
pub(super) fn diff_structs(old: &[StructDef], new: &[StructDef], out: &mut DumpDiff) {
    let old_groups = group_by(old, struct_key);
    let new_groups = group_by(new, struct_key);
    // Ancestors are followed by path in the new dump only.
    let new_by_path = group_by(new, |def| def.path.clone());

    for (key, pair) in pair_up(&old_groups, &new_groups) {
        match pair {
            Pair::Both(old_def, new_def) => {
                let mut changes = Vec::new();
                diff_header(old_def, new_def, &mut changes);
                diff_fields(&key, old_def, new_def, &mut changes, &mut out.ambiguities);
                diff_functions(
                    &key,
                    old_def,
                    new_def,
                    &new_by_path,
                    &mut changes,
                    &mut out.ambiguities,
                );
                if !changes.is_empty() {
                    out.changed_types.push(TypeDiff { key, changes });
                }
            }
            Pair::Added(_) => out.added_types.push(key),
            Pair::Removed(_) => out.removed_types.push(key),
            Pair::Ambiguous {
                old_count,
                new_count,
            } => out.ambiguities.push(Ambiguity {
                subject: AmbiguousSubject::Type,
                name: key.to_string(),
                old_count,
                new_count,
            }),
        }
    }
}

/// Kind, parent, size and alignment.
fn diff_header(old: &StructDef, new: &StructDef, changes: &mut Vec<TypeChange>) {
    if old.kind != new.kind {
        changes.push(TypeChange::KindChanged {
            old: old.kind,
            new: new.kind,
        });
    }
    if old.super_path != new.super_path {
        changes.push(TypeChange::ParentChanged {
            old: old.super_path.clone(),
            new: new.super_path.clone(),
        });
    }
    if old.size != new.size {
        changes.push(TypeChange::SizeChanged {
            old: old.size,
            new: new.size,
        });
    }
    if old.alignment != new.alignment {
        changes.push(TypeChange::AlignmentChanged {
            old: old.alignment,
            new: new.alignment,
        });
    }
}

/// Fields are matched by name. Their order is not compared.
fn diff_fields(
    owner: &TypeKey,
    old: &StructDef,
    new: &StructDef,
    changes: &mut Vec<TypeChange>,
    ambiguities: &mut Vec<Ambiguity>,
) {
    let old_groups = group_by(&old.fields, |field| field.name.clone());
    let new_groups = group_by(&new.fields, |field| field.name.clone());
    diff_field_groups(owner, &old_groups, &new_groups, changes, ambiguities);
}

fn diff_field_groups(
    owner: &TypeKey,
    old: &BTreeMap<String, Vec<&FieldDef>>,
    new: &BTreeMap<String, Vec<&FieldDef>>,
    changes: &mut Vec<TypeChange>,
    ambiguities: &mut Vec<Ambiguity>,
) {
    for (name, pair) in pair_up(old, new) {
        match pair {
            Pair::Both(old_field, new_field) => {
                let slot_changes = compare_slot(old_field, new_field);
                if !slot_changes.is_empty() {
                    changes.push(TypeChange::FieldChanged {
                        name,
                        changes: slot_changes,
                    });
                }
            }
            Pair::Added(field) => changes.push(TypeChange::FieldAdded {
                name,
                offset: field.offset,
            }),
            Pair::Removed(field) => changes.push(TypeChange::FieldRemoved {
                name,
                offset: field.offset,
            }),
            Pair::Ambiguous {
                old_count,
                new_count,
            } => ambiguities.push(Ambiguity {
                subject: AmbiguousSubject::Field {
                    owner: owner.clone(),
                },
                name,
                old_count,
                new_count,
            }),
        }
    }
}

/// Compares where and how two fields (or parameters) are stored.
///
/// The name is not compared, and neither are the property flags: callers that
/// care about flags compare the ones they need.
pub(super) fn compare_slot(old: &FieldDef, new: &FieldDef) -> Vec<SlotChange> {
    let mut changes = Vec::new();
    if old.offset != new.offset {
        changes.push(SlotChange::Offset {
            old: old.offset,
            new: new.offset,
        });
    }
    if old.ty != new.ty {
        match (old.ty.bool_bits(), new.ty.bool_bits()) {
            (Some(old_bits), Some(new_bits)) => changes.push(SlotChange::BoolBits {
                old: old_bits,
                new: new_bits,
            }),
            _ => changes.push(SlotChange::Type {
                old: old.ty.clone(),
                new: new.ty.clone(),
            }),
        }
    }
    if old.element_size != new.element_size {
        changes.push(SlotChange::ElementSize {
            old: old.element_size,
            new: new.element_size,
        });
    }
    if old.array_dim != new.array_dim {
        changes.push(SlotChange::ArrayDim {
            old: old.array_dim,
            new: new.array_dim,
        });
    }
    changes
}
