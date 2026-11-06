//! Comparison of enums.

use crate::dump::EnumDef;

use super::pairing::{Pair, group_by, pair_up};
use super::report::{Ambiguity, AmbiguousSubject, DumpDiff, EnumChange, EnumDiff, TypeKey};

/// Compares all enums of two dumps and records the result in `out`.
pub(super) fn diff_enums(old: &[EnumDef], new: &[EnumDef], out: &mut DumpDiff) {
    let key_of = |def: &EnumDef| TypeKey::new(&def.package, &def.name);
    let old_groups = group_by(old, key_of);
    let new_groups = group_by(new, key_of);

    for (key, pair) in pair_up(&old_groups, &new_groups) {
        match pair {
            Pair::Both(old_def, new_def) => {
                let mut changes = Vec::new();
                diff_enum(&key, old_def, new_def, &mut changes, &mut out.ambiguities);
                if !changes.is_empty() {
                    out.changed_enums.push(EnumDiff { key, changes });
                }
            }
            Pair::Added(_) => out.added_enums.push(key),
            Pair::Removed(_) => out.removed_enums.push(key),
            Pair::Ambiguous {
                old_count,
                new_count,
            } => out.ambiguities.push(Ambiguity {
                subject: AmbiguousSubject::Enum,
                name: key.to_string(),
                old_count,
                new_count,
            }),
        }
    }
}

fn diff_enum(
    key: &TypeKey,
    old: &EnumDef,
    new: &EnumDef,
    changes: &mut Vec<EnumChange>,
    ambiguities: &mut Vec<Ambiguity>,
) {
    if old.underlying_size != new.underlying_size {
        changes.push(EnumChange::UnderlyingSizeChanged {
            old: old.underlying_size,
            new: new.underlying_size,
        });
    }

    let old_groups = group_by(&old.values, |value| value.name.clone());
    let new_groups = group_by(&new.values, |value| value.name.clone());
    for (name, pair) in pair_up(&old_groups, &new_groups) {
        match pair {
            Pair::Both(old_value, new_value) => {
                if old_value.value != new_value.value {
                    changes.push(EnumChange::ValueChanged {
                        name,
                        old: old_value.value,
                        new: new_value.value,
                    });
                }
            }
            Pair::Added(value) => changes.push(EnumChange::ValueAdded {
                name,
                value: value.value,
            }),
            Pair::Removed(value) => changes.push(EnumChange::ValueRemoved {
                name,
                value: value.value,
            }),
            Pair::Ambiguous {
                old_count,
                new_count,
            } => ambiguities.push(Ambiguity {
                subject: AmbiguousSubject::EnumValue { owner: key.clone() },
                name,
                old_count,
                new_count,
            }),
        }
    }
}
