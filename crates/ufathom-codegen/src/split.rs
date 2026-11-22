//! The split layout: the SDK as a small Cargo workspace.
//!
//! Packages become modules of member crates, and the root crate (the facade)
//! re-exports them under the same module paths, so `sdk::engine::AActor`
//! reads the same in both layouts. The module files themselves are identical
//! in both layouts: a member crate re-exports the modules of the crates it
//! depends on at its own root, which makes every `crate::other_package::Type`
//! path in them resolve unchanged.
//!
//! Packages may reference each other cyclically, and crates may not. This
//! module groups the strongly connected components of the package reference
//! graph into one crate each, then packs small components of equal depth into
//! shared crates. Components of equal depth never depend on each other, so the
//! packing cannot create a cycle.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

use crate::build::{Built, ModuleInfo};
use crate::emit::{ACCESSORS_FEATURE, toml_string};
use crate::names::{Namespace, doc_text};

/// Folder of the member crates under the root of the SDK.
pub(crate) const MEMBER_DIR: &str = "crates";

/// Default size at which a member crate is closed, in bytes of generated
/// source: packages bigger than this get a crate of their own, smaller ones
/// are packed together. Bigger crates build in fewer, longer steps; smaller
/// ones give the compiler more to run side by side.
pub(crate) const DEFAULT_CRATE_BYTES: usize = 512 * 1024;

/// One member crate: the packages it holds and the crates it needs.
#[derive(Debug)]
pub(crate) struct CrateGroup {
    /// Cargo package name.
    pub name: String,
    /// Indices into `Built::modules`, in module-name order.
    pub modules: Vec<usize>,
    /// Indices of the crates this one depends on, in the plan's order.
    pub deps: BTreeSet<usize>,
}

impl CrateGroup {
    /// The name the crate has in Rust code.
    pub(crate) fn lib_name(&self) -> String {
        self.name.replace('-', "_")
    }

    /// The folder of the crate, relative to the root of the SDK.
    pub(crate) fn dir(&self) -> String {
        format!("{MEMBER_DIR}/{}", self.name)
    }
}

/// The strongly connected components of a directed graph, dependencies first:
/// a component comes after every component it has an edge into. Edges to
/// indices outside the graph are ignored.
///
/// Iterative Tarjan, so a long chain of packages cannot exhaust the stack.
pub(crate) fn strongly_connected(adjacency: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let count = adjacency.len();
    let mut index: Vec<Option<usize>> = vec![None; count];
    let mut low = vec![0usize; count];
    let mut on_stack = vec![false; count];
    let mut stack: Vec<usize> = Vec::new();
    let mut next = 0usize;
    let mut components: Vec<Vec<usize>> = Vec::new();

    for root in 0..count {
        if index.get(root).copied().flatten().is_some() {
            continue;
        }
        let mut frames: Vec<(usize, usize)> = vec![(root, 0)];
        visit(
            root,
            &mut next,
            &mut index,
            &mut low,
            &mut on_stack,
            &mut stack,
        );
        while let Some(frame) = frames.last_mut() {
            let (node, edge) = *frame;
            let target = adjacency
                .get(node)
                .and_then(|edges| edges.get(edge))
                .copied();
            if let Some(target) = target {
                frame.1 += 1;
                if target >= count {
                    continue;
                }
                match index.get(target).copied().flatten() {
                    None => {
                        visit(
                            target,
                            &mut next,
                            &mut index,
                            &mut low,
                            &mut on_stack,
                            &mut stack,
                        );
                        frames.push((target, 0));
                    }
                    Some(target_index) => {
                        if on_stack.get(target).copied().unwrap_or(false) {
                            lower(&mut low, node, target_index);
                        }
                    }
                }
                continue;
            }
            frames.pop();
            let node_low = low.get(node).copied().unwrap_or(0);
            if index.get(node).copied().flatten() == Some(node_low) {
                let mut component = Vec::new();
                while let Some(member) = stack.pop() {
                    if let Some(flag) = on_stack.get_mut(member) {
                        *flag = false;
                    }
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                component.sort_unstable();
                components.push(component);
            }
            if let Some(&(parent, _)) = frames.last() {
                lower(&mut low, parent, node_low);
            }
        }
    }
    components
}

fn visit(
    node: usize,
    next: &mut usize,
    index: &mut [Option<usize>],
    low: &mut [usize],
    on_stack: &mut [bool],
    stack: &mut Vec<usize>,
) {
    if let Some(slot) = index.get_mut(node) {
        *slot = Some(*next);
    }
    if let Some(slot) = low.get_mut(node) {
        *slot = *next;
    }
    if let Some(flag) = on_stack.get_mut(node) {
        *flag = true;
    }
    *next += 1;
    stack.push(node);
}

fn lower(low: &mut [usize], node: usize, value: usize) {
    if let Some(slot) = low.get_mut(node) {
        *slot = (*slot).min(value);
    }
}

/// A strongly connected component of packages, with what the packing needs.
struct Component {
    modules: Vec<usize>,
    weight: usize,
    /// Smallest module name, for stable ordering and naming.
    first: String,
}

/// Groups the packages into crates.
///
/// - Every strongly connected component stays in one crate.
/// - A component of at least `target` bytes gets a crate of its own.
/// - Smaller components of the same depth (the longest chain of dependencies
///   below them) are packed, in name order, until a crate reaches `target`.
///
/// `weights[i]` is the size of module `i`. `reserved` are package names that
/// the crates must not take. The result is ordered by depth, then name, so a
/// crate comes after all its dependencies.
pub(crate) fn plan_crates(
    modules: &[ModuleInfo],
    weights: &[usize],
    target: usize,
    prefix: &str,
    reserved: &[&str],
) -> Vec<CrateGroup> {
    let by_name: BTreeMap<&str, usize> = modules
        .iter()
        .enumerate()
        .map(|(i, m)| (m.name.as_str(), i))
        .collect();
    let adjacency: Vec<Vec<usize>> = modules
        .iter()
        .map(|m| {
            m.deps
                .iter()
                .filter_map(|d| by_name.get(d.as_str()).copied())
                .collect()
        })
        .collect();
    let components = strongly_connected(&adjacency);

    let mut component_of = vec![0usize; modules.len()];
    for (c, members) in components.iter().enumerate() {
        for &m in members {
            if let Some(slot) = component_of.get_mut(m) {
                *slot = c;
            }
        }
    }
    // Components come dependencies first, so every dependency has its depth.
    let mut depth = vec![0usize; components.len()];
    for (c, members) in components.iter().enumerate() {
        let mut here = 0usize;
        for &m in members {
            for &d in adjacency.get(m).into_iter().flatten() {
                let other = component_of.get(d).copied().unwrap_or(c);
                if other != c {
                    here = here.max(depth.get(other).copied().unwrap_or(0) + 1);
                }
            }
        }
        if let Some(slot) = depth.get_mut(c) {
            *slot = here;
        }
    }

    let mut by_depth: BTreeMap<usize, Vec<Component>> = BTreeMap::new();
    for (c, members) in components.iter().enumerate() {
        let name_of = |m: &usize| modules.get(*m).map(|x| x.name.clone()).unwrap_or_default();
        let first = members.iter().map(name_of).min().unwrap_or_default();
        let weight = members
            .iter()
            .map(|m| weights.get(*m).copied().unwrap_or(0))
            .sum();
        by_depth
            .entry(depth.get(c).copied().unwrap_or(0))
            .or_default()
            .push(Component {
                modules: members.clone(),
                weight,
                first,
            });
    }

    let mut names = Namespace::with_reserved(reserved);
    let mut groups: Vec<CrateGroup> = Vec::new();
    let mut close = |bin: &mut Vec<usize>, first: &mut String, packed: &mut usize| {
        if bin.is_empty() {
            return;
        }
        bin.sort_by_key(|m| modules.get(*m).map(|x| x.name.clone()));
        let suffix = if bin.len() == 1 { "" } else { "-pkgs" };
        let name = names.claim(&member_name(prefix, first, suffix));
        groups.push(CrateGroup {
            name,
            modules: std::mem::take(bin),
            deps: BTreeSet::new(),
        });
        first.clear();
        *packed = 0;
    };
    for level in by_depth.values_mut() {
        level.sort_by(|a, b| a.first.cmp(&b.first));
        let mut bin: Vec<usize> = Vec::new();
        let mut first = String::new();
        let mut packed = 0usize;
        for component in level.iter() {
            if component.weight >= target {
                close(&mut bin, &mut first, &mut packed);
                let mut own = component.modules.clone();
                let mut own_first = component.first.clone();
                let mut own_packed = 0;
                close(&mut own, &mut own_first, &mut own_packed);
                continue;
            }
            if !bin.is_empty() && packed + component.weight > target {
                close(&mut bin, &mut first, &mut packed);
            }
            if bin.is_empty() {
                first.clone_from(&component.first);
            }
            bin.extend(component.modules.iter().copied());
            packed += component.weight;
        }
        close(&mut bin, &mut first, &mut packed);
    }

    let mut group_of = vec![0usize; modules.len()];
    for (g, group) in groups.iter().enumerate() {
        for &m in &group.modules {
            if let Some(slot) = group_of.get_mut(m) {
                *slot = g;
            }
        }
    }
    for (g, group) in groups.iter_mut().enumerate() {
        for &m in &group.modules {
            for &d in adjacency.get(m).into_iter().flatten() {
                let other = group_of.get(d).copied().unwrap_or(g);
                if other != g {
                    group.deps.insert(other);
                }
            }
        }
    }
    groups
}

/// `prefix-first-module` with `_` written as `-`, then `suffix`.
fn member_name(prefix: &str, first: &str, suffix: &str) -> String {
    let module = first.replace('_', "-");
    format!("{prefix}-{}{suffix}", module.trim_matches('-'))
}

/// Which crate holds each module: module name to Rust crate name.
pub(crate) fn exports(built: &Built, groups: &[CrateGroup]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for group in groups {
        for module in group.modules.iter().filter_map(|m| built.modules.get(*m)) {
            out.insert(module.name.clone(), group.lib_name());
        }
    }
    out
}

/// The `[dependencies]` lines of the root crate for the member crates.
pub(crate) fn root_dependency_lines(groups: &[CrateGroup]) -> Vec<String> {
    groups
        .iter()
        .map(|g| {
            format!(
                "{} = {{ path = {}, optional = true }}",
                g.name,
                toml_string(&g.dir())
            )
        })
        .collect()
}

/// A feature of the root crate that passes `feature` on to every member crate
/// that is enabled.
pub(crate) fn forward_to_members(groups: &[CrateGroup], feature: &str) -> Vec<String> {
    groups
        .iter()
        .map(|g| toml_string(&format!("{}?/{feature}", g.name)))
        .collect()
}

/// The text of `crates/<name>/Cargo.toml`.
///
/// `runtime` is the dependency value of `ufathom-runtime` as written from the
/// crate's own folder.
pub(crate) fn member_manifest(
    group: &CrateGroup,
    groups: &[CrateGroup],
    crate_version: &str,
    runtime: &str,
) -> String {
    let deps: Vec<&CrateGroup> = group.deps.iter().filter_map(|d| groups.get(*d)).collect();
    let mut out = String::new();
    let _ = writeln!(out, "[package]");
    let _ = writeln!(out, "name = {}", toml_string(&group.name));
    let _ = writeln!(out, "version = {}", toml_string(crate_version));
    let _ = writeln!(out, "edition = \"2021\"");
    let _ = writeln!(
        out,
        "description = \"Packages of a Rust SDK generated by ufathom-codegen.\""
    );
    let _ = writeln!(out, "publish = false");
    let _ = writeln!(out);
    let _ = writeln!(out, "[dependencies]");
    let _ = writeln!(out, "ufathom-runtime = {runtime}");
    for dep in &deps {
        let _ = writeln!(
            out,
            "{} = {{ path = {} }}",
            dep.name,
            toml_string(&format!("../{}", dep.name))
        );
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "[features]");
    let mut inproc = vec![toml_string("ufathom-runtime/inproc")];
    inproc.extend(
        deps.iter()
            .map(|d| toml_string(&format!("{}/inproc", d.name))),
    );
    let _ = writeln!(out, "inproc = [{}]", inproc.join(", "));
    let accessors: Vec<String> = deps
        .iter()
        .map(|d| toml_string(&format!("{}/{ACCESSORS_FEATURE}", d.name)))
        .collect();
    let _ = writeln!(out, "{ACCESSORS_FEATURE} = [{}]", accessors.join(", "));
    let _ = writeln!(out);
    // An explicit empty table: the workspace lints are for the vendored
    // runtime, and newer cargo warns about a member that does not say so.
    let _ = writeln!(
        out,
        "# The workspace lints are for the vendored runtime, not for generated code."
    );
    let _ = writeln!(out, "[lints]");
    out
}

/// The text of `crates/<name>/src/lib.rs`.
pub(crate) fn member_lib_source(
    built: &Built,
    group: &CrateGroup,
    groups: &[CrateGroup],
    fname_alias: &str,
) -> String {
    let own: BTreeSet<&str> = group
        .modules
        .iter()
        .filter_map(|m| built.modules.get(*m))
        .map(|m| m.name.as_str())
        .collect();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "//! Packages of a Rust SDK generated by ufathom-codegen. Do not edit."
    );
    let _ = writeln!(out, "//!");
    let packages: Vec<String> = group
        .modules
        .iter()
        .filter_map(|m| built.modules.get(*m))
        .map(|m| format!("`{}`", doc_text(&m.package)))
        .collect();
    let _ = writeln!(out, "//! Holds {}.", packages.join(", "));
    let _ = writeln!(out, "//!");
    let _ = writeln!(
        out,
        "//! The packages this crate needs from other crates are re-exported here under"
    );
    let _ = writeln!(
        out,
        "//! their module names, so a path such as `crate::engine::AActor` works in every"
    );
    let _ = writeln!(out, "//! module.");
    let _ = writeln!(out);
    let _ = writeln!(out, "#![allow(");
    let _ = writeln!(out, "    non_camel_case_types,");
    let _ = writeln!(out, "    non_snake_case,");
    let _ = writeln!(out, "    non_upper_case_globals,");
    let _ = writeln!(out, "    dead_code,");
    let _ = writeln!(out, "    unused_imports,");
    let _ = writeln!(out, "    clippy::all,");
    let _ = writeln!(out, "    clippy::pedantic,");
    let _ = writeln!(out, "    clippy::nursery");
    let _ = writeln!(out, ")]");
    let _ = writeln!(out);
    let _ = writeln!(out, "/// The `FName` of this build.");
    let _ = writeln!(out, "pub type FName = {fname_alias};");
    let _ = writeln!(out);
    for module in group.modules.iter().filter_map(|m| built.modules.get(*m)) {
        let _ = writeln!(out, "pub mod {};", module.name);
    }
    let owner = exports(built, groups);
    let mut needed: BTreeSet<&str> = BTreeSet::new();
    for module in group.modules.iter().filter_map(|m| built.modules.get(*m)) {
        for dep in &module.deps {
            needed.insert(dep.as_str());
        }
    }
    let mut wrote_header = false;
    for dep in needed {
        if own.contains(dep) {
            continue;
        }
        let Some(lib) = owner.get(dep) else { continue };
        if !wrote_header {
            let _ = writeln!(out);
            let _ = writeln!(out, "// Packages of the crates this one depends on.");
            wrote_header = true;
        }
        let _ = writeln!(out, "pub use ::{lib}::{dep};");
    }
    out
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn module(name: &str, deps: &[&str]) -> ModuleInfo {
        ModuleInfo {
            package: format!("/Script/{name}"),
            name: name.to_owned(),
            deps: deps
                .iter()
                .map(|d| (*d).to_owned())
                .collect::<BTreeSet<_>>(),
        }
    }

    fn names_of(groups: &[CrateGroup], modules: &[ModuleInfo]) -> Vec<Vec<String>> {
        groups
            .iter()
            .map(|g| g.modules.iter().map(|m| modules[*m].name.clone()).collect())
            .collect()
    }

    #[test]
    fn components_come_dependencies_first() {
        // 0 -> 1 -> 2, 2 -> 1 (a cycle), 3 alone.
        let adjacency = vec![vec![1], vec![2], vec![1], vec![]];
        let components = strongly_connected(&adjacency);
        assert_eq!(components, vec![vec![1, 2], vec![0], vec![3]]);
    }

    #[test]
    fn a_long_chain_does_not_overflow_the_stack() {
        let n = 200_000;
        let adjacency: Vec<Vec<usize>> = (0..n)
            .map(|i| if i + 1 < n { vec![i + 1] } else { vec![] })
            .collect();
        let components = strongly_connected(&adjacency);
        assert_eq!(components.len(), n);
        assert_eq!(components.first(), Some(&vec![n - 1]));
    }

    #[test]
    fn edges_outside_the_graph_are_ignored() {
        let components = strongly_connected(&[vec![7], vec![]]);
        assert_eq!(components.len(), 2);
    }

    #[test]
    fn a_package_cycle_ends_up_in_one_crate() {
        let modules = vec![
            module("core", &[]),
            module("engine", &["core", "umg"]),
            module("umg", &["core", "engine"]),
            module("game", &["engine", "umg"]),
        ];
        // Every package is bigger than the target: one crate per component.
        let groups = plan_crates(&modules, &[10, 10, 10, 10], 1, "sdk", &[]);
        assert_eq!(
            names_of(&groups, &modules),
            vec![
                vec!["core".to_owned()],
                vec!["engine".to_owned(), "umg".to_owned()],
                vec!["game".to_owned()],
            ]
        );
        let names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["sdk-core", "sdk-engine-pkgs", "sdk-game"]);
        // `game` needs the cycle's crate, which needs `core`.
        assert_eq!(groups[2].deps, BTreeSet::from([1]));
        assert_eq!(groups[1].deps, BTreeSet::from([0]));
    }

    #[test]
    fn small_packages_of_one_depth_share_a_crate() {
        let modules = vec![
            module("base", &[]),
            module("a", &["base"]),
            module("b", &["base"]),
            module("c", &["base"]),
            module("d", &["a"]),
        ];
        let groups = plan_crates(&modules, &[100, 1, 1, 1, 1], 50, "sdk", &[]);
        assert_eq!(
            names_of(&groups, &modules),
            vec![
                vec!["base".to_owned()],
                vec!["a".to_owned(), "b".to_owned(), "c".to_owned()],
                vec!["d".to_owned()],
            ]
        );
    }

    #[test]
    fn a_crate_is_closed_when_the_next_package_does_not_fit() {
        let modules: Vec<ModuleInfo> = ["a", "b", "c", "d"]
            .iter()
            .map(|n| module(n, &[]))
            .collect();
        let groups = plan_crates(&modules, &[30, 30, 30, 30], 70, "sdk", &[]);
        assert_eq!(
            names_of(&groups, &modules),
            vec![
                vec!["a".to_owned(), "b".to_owned()],
                vec!["c".to_owned(), "d".to_owned()],
            ]
        );
    }

    #[test]
    fn crate_names_avoid_reserved_names_and_each_other() {
        let modules = vec![module("runtime", &[]), module("x_", &[]), module("x", &[])];
        let groups = plan_crates(&modules, &[1, 1, 1], 0, "ufathom", &["ufathom-runtime"]);
        let mut names: Vec<&str> = groups.iter().map(|g| g.name.as_str()).collect();
        assert!(!names.contains(&"ufathom-runtime"));
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), 3);
    }

    #[test]
    fn packing_never_makes_a_cycle_between_crates() {
        // A pseudo-random graph with cycles, packed into few crates.
        let n = 300usize;
        let mut state = 0x2545_f491_u64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            usize::try_from(state >> 33).unwrap()
        };
        let names: Vec<String> = (0..n).map(|i| format!("m{i:03}")).collect();
        let modules: Vec<ModuleInfo> = (0..n)
            .map(|i| {
                let deps: Vec<&str> = (0..3)
                    .map(|_| next() % n)
                    .filter(|d| *d != i)
                    .map(|d| names[d].as_str())
                    .collect();
                module(&names[i], &deps)
            })
            .collect();
        let weights: Vec<usize> = (0..n).map(|_| 1 + next() % 40).collect();
        for target in [0, 60, 500, 100_000] {
            let groups = plan_crates(&modules, &weights, target, "sdk", &[]);
            let total: usize = groups.iter().map(|g| g.modules.len()).sum();
            assert_eq!(total, n, "every module is in exactly one crate");
            // The groups are ordered dependencies first, which a cycle between
            // crates would break.
            for (i, group) in groups.iter().enumerate() {
                assert!(group.deps.iter().all(|d| *d < i), "target {target}");
            }
        }
    }
}
