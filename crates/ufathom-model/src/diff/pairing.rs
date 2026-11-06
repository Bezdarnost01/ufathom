//! Matching two keyed collections without ever choosing between duplicates.

use std::collections::{BTreeMap, BTreeSet};

/// Groups `items` by key. Every item is kept, so duplicates stay visible.
pub(super) fn group_by<'a, T, K: Ord>(
    items: &'a [T],
    key: impl Fn(&T) -> K,
) -> BTreeMap<K, Vec<&'a T>> {
    let mut groups: BTreeMap<K, Vec<&'a T>> = BTreeMap::new();
    for item in items {
        groups.entry(key(item)).or_default().push(item);
    }
    groups
}

/// How one key relates across the old and the new side.
pub(super) enum Pair<'a, T> {
    /// Exactly one item on each side.
    Both(&'a T, &'a T),
    /// Exactly one item, only on the new side.
    Added(&'a T),
    /// Exactly one item, only on the old side.
    Removed(&'a T),
    /// More than one item on at least one side. Nothing is compared.
    Ambiguous {
        /// Items on the old side.
        old_count: usize,
        /// Items on the new side.
        new_count: usize,
    },
}

/// Pairs the groups by key, in ascending key order.
///
/// A key with more than one item on either side is `Ambiguous`: picking one
/// of the candidates would report differences that may not exist.
pub(super) fn pair_up<'a, T, K: Ord + Clone>(
    old: &BTreeMap<K, Vec<&'a T>>,
    new: &BTreeMap<K, Vec<&'a T>>,
) -> Vec<(K, Pair<'a, T>)> {
    let keys: BTreeSet<&K> = old.keys().chain(new.keys()).collect();
    let mut pairs = Vec::with_capacity(keys.len());
    for key in keys {
        let old_items = old.get(key).map_or(&[][..], Vec::as_slice);
        let new_items = new.get(key).map_or(&[][..], Vec::as_slice);
        let pair = match (old_items, new_items) {
            ([old_one], [new_one]) => Pair::Both(*old_one, *new_one),
            ([], [new_one]) => Pair::Added(*new_one),
            ([old_one], []) => Pair::Removed(*old_one),
            _ => Pair::Ambiguous {
                old_count: old_items.len(),
                new_count: new_items.len(),
            },
        };
        pairs.push((key.clone(), pair));
    }
    pairs
}
