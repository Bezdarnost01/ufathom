//! Step 4, second half: `UStruct::Children` and `UField::Next`.
//!
//! Both are found together, because a struct points at several lists of its
//! own members and only one of them is `Children`:
//!
//! - `Children` heads the list of every member: properties and, for a class,
//!   its functions, joined by `UField::Next`.
//! - `PropertyLink`, `RefLink`, `DestructorLink` and `PostConstructLink`
//!   head chains of properties only, joined by the `*Next` pointers of the
//!   property. Their head is often a property whose outer is the struct, so
//!   the outer test alone does not tell them from `Children`.
//!
//! What does tell them apart is how much of the struct each list reaches
//! through `UField::Next`. Starting at `Children`, it reaches every property
//! and function. Starting at a link chain head, it reaches only the members
//! declared after that head, so the real `Children` covers the most members.
//! Only when the coverage is identical does the position decide: the engine
//! declares `Children` right after `SuperStruct`.

use std::collections::HashSet;

use crate::ctx::Ctx;
use crate::error::ScanError;
use crate::limits::{MAX_CHAIN_STEPS, MAX_REFLECTION_OFFSET, MAX_UOBJECT_OFFSET};
use crate::structs::{FIRST_REFLECTION_OFFSET, Sizes, offsets_8};
use crate::universe::{Anchors, Obj, Universe};

/// A pointer whose target's outer is the owning struct.
#[derive(Debug, Clone, Copy)]
struct OuterCandidate {
    offset: u32,
    matched: usize,
    non_null: usize,
}

/// How far the lists headed at one candidate reach through the best `Next`.
#[derive(Debug, Clone, Copy)]
struct Reach {
    next: u32,
    members: usize,
    ended: usize,
}

/// The pointers into a struct whose target's outer is that struct.
fn outer_candidates(
    ctx: &Ctx<'_>,
    universe: &Universe,
    structs: &[Obj],
    anchors: &Anchors,
    sizes: Sizes,
    super_offset: u32,
) -> Vec<OuterCandidate> {
    let mut found = Vec::new();
    for offset in offsets_8(FIRST_REFLECTION_OFFSET, MAX_REFLECTION_OFFSET) {
        if offset == super_offset {
            continue;
        }
        let mut non_null = 0usize;
        let mut matched = 0usize;
        for owner in structs {
            if offset + 8 > sizes.of(owner, anchors) {
                continue;
            }
            let Some(value) = ctx.u64(owner.addr + u64::from(offset)) else {
                continue;
            };
            if value == 0 {
                continue;
            }
            non_null += 1;
            if universe
                .get(value)
                .is_some_and(|member| member.outer == owner.addr)
            {
                matched += 1;
            }
        }
        // Two owners suffice when every one of them agrees: a modern image
        // whose classes with functions are few still shows the pointer.
        let enough = non_null >= 3 && matched * 10 >= non_null * 9 || non_null == 2 && matched == 2;
        if enough {
            found.push(OuterCandidate {
                offset,
                matched,
                non_null,
            });
        }
    }
    found
}

/// Walks the member list from `head` through `next` while the outer stays
/// `owner`. Returns the number of members and whether the list ended at null.
fn walk_members(
    ctx: &Ctx<'_>,
    universe: &Universe,
    owner: u64,
    head: u64,
    next: u32,
) -> (usize, bool) {
    let mut seen = HashSet::new();
    let mut current = head;
    while current != 0 && seen.len() < MAX_CHAIN_STEPS {
        let belongs = universe.get(current).is_some_and(|o| o.outer == owner);
        if !belongs || !seen.insert(current) {
            return (seen.len(), false);
        }
        match ctx.u64(current + u64::from(next)) {
            Some(link) => current = link,
            None => return (seen.len(), false),
        }
    }
    (seen.len(), current == 0)
}

/// `(members, ended)` per `Next` offset for the lists headed at `children`,
/// best first, and the number of lists. The `Next` offset is accepted when
/// the best entry is clearly ahead of the rest.
struct NextScan {
    scored: Vec<(u32, usize, usize)>,
    lists: usize,
}

impl NextScan {
    fn new(ctx: &Ctx<'_>, universe: &Universe, structs: &[Obj], children: u32) -> Self {
        let heads: Vec<(u64, u64)> = structs
            .iter()
            .filter_map(|owner| {
                let head = ctx.u64(owner.addr + u64::from(children))?;
                (head != 0).then_some((owner.addr, head))
            })
            .collect();
        let mut scored: Vec<(u32, usize, usize)> = Vec::new();
        for offset in offsets_8(8, MAX_UOBJECT_OFFSET + 0x40) {
            let mut members = 0usize;
            let mut ended = 0usize;
            for &(owner, head) in &heads {
                let (count, at_null) = walk_members(ctx, universe, owner, head, offset);
                members += count;
                ended += usize::from(at_null);
            }
            scored.push((offset, members, ended));
        }
        scored
            .sort_by_key(|&(offset, members, ended)| (std::cmp::Reverse((members, ended)), offset));
        Self {
            scored,
            lists: heads.len(),
        }
    }

    /// The `Next` offset the lists agree on, if one stands out.
    fn accepted(&self) -> Option<Reach> {
        match self.scored.as_slice() {
            [(offset, members, ended), rest @ ..]
                if *members > self.lists * 2
                    && *ended * 10 >= self.lists * 9
                    && rest.first().is_none_or(|r| (r.1, r.2) < (*members, *ended)) =>
            {
                Some(Reach {
                    next: *offset,
                    members: *members,
                    ended: *ended,
                })
            }
            _ => None,
        }
    }
}

/// The candidate with the largest reach. `None` when several share it and
/// `Children` follows `SuperStruct` in at most one of them, or when the
/// reach of none can be measured.
fn widest_reach(
    reaches: &[(OuterCandidate, Reach)],
    super_offset: u32,
) -> Result<(OuterCandidate, Reach), Vec<u32>> {
    let best = reaches
        .iter()
        .map(|(_, r)| (r.members, r.ended))
        .max()
        .unwrap_or((0, 0));
    let tied: Vec<&(OuterCandidate, Reach)> = reaches
        .iter()
        .filter(|(_, r)| (r.members, r.ended) == best)
        .collect();
    match tied.as_slice() {
        [only] => return Ok(**only),
        [] => {}
        many => {
            let adjacent: Vec<&&(OuterCandidate, Reach)> = many
                .iter()
                .filter(|(c, _)| c.offset == super_offset + 8)
                .collect();
            if let [only] = adjacent.as_slice() {
                return Ok(***only);
            }
        }
    }
    Err(tied.iter().map(|(c, _)| c.offset).collect())
}

/// Solves `UStruct::Children` and `UField::Next`.
///
/// # Errors
///
/// [`ScanError::Ambiguous`] when several pointers head equally wide member
/// lists and none follows `SuperStruct`; [`ScanError::MissingEvidence`] when
/// no pointer or no `Next` offset fits.
pub(crate) fn solve_children_and_next(
    ctx: &Ctx<'_>,
    universe: &Universe,
    structs: &[Obj],
    anchors: &Anchors,
    sizes: Sizes,
    super_offset: u32,
) -> Result<(u32, u32), ScanError> {
    let found = outer_candidates(ctx, universe, structs, anchors, sizes, super_offset);
    let offsets: Vec<u32> = found.iter().map(|c| c.offset).collect();
    let overridden = ctx.options.overrides.get("ustruct.children").is_some();

    let (chosen, scan) = if offsets.len() <= 1 || overridden {
        let offset = ctx.resolve_among("ustruct.children", &offsets)?;
        let chosen = found
            .iter()
            .find(|c| c.offset == offset)
            .copied()
            .unwrap_or(OuterCandidate {
                offset,
                matched: 0,
                non_null: 0,
            });
        (chosen, NextScan::new(ctx, universe, structs, offset))
    } else {
        let scans: Vec<(OuterCandidate, NextScan)> = found
            .iter()
            .map(|&c| (c, NextScan::new(ctx, universe, structs, c.offset)))
            .collect();
        let reaches: Vec<(OuterCandidate, Reach)> = scans
            .iter()
            .filter_map(|(c, scan)| Some((*c, scan.accepted()?)))
            .collect();
        match widest_reach(&reaches, super_offset) {
            Ok((chosen, _)) => {
                let scan = scans
                    .into_iter()
                    .find(|(c, _)| c.offset == chosen.offset)
                    .map(|(_, scan)| scan);
                match scan {
                    Some(scan) => (chosen, scan),
                    None => return Err(ScanError::missing("ustruct.children", "no candidate")),
                }
            }
            Err(tied) => {
                let among = if tied.is_empty() { &offsets } else { &tied };
                return Err(ScanError::Ambiguous {
                    field: "ustruct.children",
                    candidates: among
                        .iter()
                        .map(|c| format!("{c:#x}"))
                        .collect::<Vec<_>>()
                        .join(", "),
                });
            }
        }
    };

    ctx.evidence(
        "ustruct.children",
        u64::from(chosen.offset),
        if offsets.len() > 1 {
            format!(
                "pointer whose target's outer is the owning struct, heading the widest member list of {} such pointers",
                offsets.len()
            )
        } else {
            "pointer whose target's outer is the owning struct".to_owned()
        },
        chosen.non_null,
        chosen.matched,
    );
    let next = ctx.resolve("ufield.next", scan.accepted().map(|r| r.next), || {
        format!("no offset walks the member lists of {} owners", scan.lists)
    })?;
    let (members, ended) = scan
        .scored
        .iter()
        .find(|&&(o, _, _)| o == next)
        .map_or((0, 0), |&(_, m, e)| (m, e));
    ctx.evidence(
        "ufield.next",
        u64::from(next),
        format!(
            "walks {members} members of {} lists, {ended} ending at null, all sharing the owner as outer",
            scan.lists
        ),
        scan.lists,
        ended,
    );
    Ok((chosen.offset, next))
}
