//! Comparison of function ABIs.

use std::collections::{BTreeMap, BTreeSet};

use crate::dump::{FieldDef, FunctionDef, StructDef};
use crate::flags::{
    CPF_CONST_PARM, CPF_OUT_PARM, CPF_REFERENCE_PARM, CPF_RETURN_PARM, FUNC_NETWORK_MASK,
};

use super::pairing::{Pair, group_by, pair_up};
use super::report::{
    Ambiguity, AmbiguousSubject, FunctionChange, ParamChange, ParamFlag, TypeChange, TypeKey,
};
use super::structs::{compare_slot, struct_key};

/// Parameter flags that belong to the calling convention.
const COMPARED_PARAM_FLAGS: [(ParamFlag, u64); 4] = [
    (ParamFlag::Out, CPF_OUT_PARM),
    (ParamFlag::Return, CPF_RETURN_PARM),
    (ParamFlag::Reference, CPF_REFERENCE_PARM),
    (ParamFlag::Const, CPF_CONST_PARM),
];

/// Result of searching the ancestors of a class for a function.
enum AncestorSearch<'a> {
    /// Exactly one function of that name in the nearest ancestor that has any.
    Found(&'a StructDef, &'a FunctionDef),
    /// No ancestor declares it, or the chain ends or loops.
    NotFound,
    /// The search met a duplicate and refuses to choose.
    Ambiguous(Ambiguity),
}

/// Compares the functions of two matched types.
///
/// Functions are matched by name. A function missing from `new` is looked up
/// in the ancestors of `new`: found there it is reported as moved, not
/// removed, because calling it on the class still works.
pub(super) fn diff_functions(
    owner: &TypeKey,
    old: &StructDef,
    new: &StructDef,
    new_by_path: &BTreeMap<String, Vec<&StructDef>>,
    changes: &mut Vec<TypeChange>,
    ambiguities: &mut Vec<Ambiguity>,
) {
    let old_groups = group_by(&old.functions, |function| function.name.clone());
    let new_groups = group_by(&new.functions, |function| function.name.clone());

    for (name, pair) in pair_up(&old_groups, &new_groups) {
        match pair {
            Pair::Both(old_fn, new_fn) => push_abi_changes(&name, old_fn, new_fn, changes),
            Pair::Added(_) => changes.push(TypeChange::FunctionAdded { name }),
            Pair::Removed(old_fn) => match find_in_ancestors(new, &name, new_by_path) {
                AncestorSearch::Found(ancestor, new_fn) => {
                    changes.push(TypeChange::FunctionMoved {
                        name: name.clone(),
                        to: struct_key(ancestor),
                    });
                    push_abi_changes(&name, old_fn, new_fn, changes);
                }
                AncestorSearch::NotFound => changes.push(TypeChange::FunctionRemoved { name }),
                AncestorSearch::Ambiguous(ambiguity) => ambiguities.push(ambiguity),
            },
            Pair::Ambiguous {
                old_count,
                new_count,
            } => ambiguities.push(Ambiguity {
                subject: AmbiguousSubject::Function {
                    owner: owner.clone(),
                },
                name,
                old_count,
                new_count,
            }),
        }
    }
}

fn push_abi_changes(
    name: &str,
    old: &FunctionDef,
    new: &FunctionDef,
    changes: &mut Vec<TypeChange>,
) {
    let abi_changes = compare_functions(old, new);
    if !abi_changes.is_empty() {
        changes.push(TypeChange::FunctionChanged {
            name: name.to_owned(),
            changes: abi_changes,
        });
    }
}

/// Walks up the parents of `start` looking for a function called `name`.
///
/// Stops at the first ancestor that declares the name. A cycle in the parent
/// chain ends the search instead of looping.
fn find_in_ancestors<'a>(
    start: &'a StructDef,
    name: &str,
    by_path: &'a BTreeMap<String, Vec<&'a StructDef>>,
) -> AncestorSearch<'a> {
    let mut visited: BTreeSet<&str> = BTreeSet::new();
    visited.insert(start.path.as_str());
    let mut current = start;

    while let Some(parent_path) = current.super_path.as_deref() {
        if !visited.insert(parent_path) {
            return AncestorSearch::NotFound;
        }
        let parent: &'a StructDef = match by_path.get(parent_path).map(Vec::as_slice) {
            None | Some([]) => return AncestorSearch::NotFound,
            Some([only]) => only,
            Some(several) => {
                return AncestorSearch::Ambiguous(Ambiguity {
                    subject: AmbiguousSubject::AncestorPath,
                    name: parent_path.to_owned(),
                    old_count: 0,
                    new_count: several.len(),
                });
            }
        };

        let candidates: Vec<&'a FunctionDef> = parent
            .functions
            .iter()
            .filter(|function| function.name == name)
            .collect();
        match candidates.as_slice() {
            [] => {}
            [only] => return AncestorSearch::Found(parent, only),
            several => {
                return AncestorSearch::Ambiguous(Ambiguity {
                    subject: AmbiguousSubject::AncestorFunction {
                        owner: struct_key(parent),
                    },
                    name: name.to_owned(),
                    old_count: 0,
                    new_count: several.len(),
                });
            }
        }
        current = parent;
    }
    AncestorSearch::NotFound
}

/// Compares two functions by ABI.
///
/// Parameters are matched by position and compared by layout. A renamed
/// parameter is not a change: the name is not part of the calling convention.
fn compare_functions(old: &FunctionDef, new: &FunctionDef) -> Vec<FunctionChange> {
    let mut changes = Vec::new();
    if old.params_size != new.params_size {
        changes.push(FunctionChange::ParamsSize {
            old: old.params_size,
            new: new.params_size,
        });
    }
    let old_network = old.flags & FUNC_NETWORK_MASK;
    let new_network = new.flags & FUNC_NETWORK_MASK;
    if old_network != new_network {
        changes.push(FunctionChange::NetworkFlags {
            old: old_network,
            new: new_network,
        });
    }
    if old.params.len() != new.params.len() {
        changes.push(FunctionChange::ParamCount {
            old: old.params.len(),
            new: new.params.len(),
        });
    }
    for (index, (old_param, new_param)) in old.params.iter().zip(&new.params).enumerate() {
        let param_changes = compare_param(old_param, new_param);
        if !param_changes.is_empty() {
            changes.push(FunctionChange::Param {
                index,
                name: new_param.name.clone(),
                changes: param_changes,
            });
        }
    }
    changes
}

fn compare_param(old: &FieldDef, new: &FieldDef) -> Vec<ParamChange> {
    let mut changes: Vec<ParamChange> = compare_slot(old, new)
        .into_iter()
        .map(ParamChange::Slot)
        .collect();
    for (flag, mask) in COMPARED_PARAM_FLAGS {
        let was = old.flags & mask != 0;
        let now = new.flags & mask != 0;
        if was != now {
            changes.push(ParamChange::Flag {
                flag,
                old: was,
                new: now,
            });
        }
    }
    changes
}
