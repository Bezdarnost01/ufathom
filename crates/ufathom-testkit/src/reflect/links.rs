//! The link chains an engine builds when it links a struct: `PropertyLink`,
//! `RefLink`, `DestructorLink` and `PostConstructLink`, threaded through the
//! `*Next` pointers of the properties.
//!
//! A real engine fills these for every linked struct, and they are the reason
//! `Children` is not the only pointer in a `UStruct` that leads to members of
//! that struct. Each chain starts with the struct's own properties and ends in
//! the chain of its parent, so a subclass chain flows into the parent chain.

use ufathom_model::{PropertyType, StructDef};

use crate::profile::{PropertyLinks, StructLinks};
use crate::schema::{CPF_CONFIG, CPF_INSTANCED_REFERENCE};

use super::{Kind, Writer};

/// Deepest nesting of structs and containers a classification follows.
const MAX_NESTING: usize = 8;
/// Most parents followed when collecting inherited properties.
const MAX_PARENTS: usize = 64;

/// One of the four chains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chain {
    /// Every property.
    Property,
    /// Properties that hold object references.
    Ref,
    /// Properties that need destruction.
    Destructor,
    /// Config and instanced properties.
    PostConstruct,
}

const CHAINS: [Chain; 4] = [
    Chain::Property,
    Chain::Ref,
    Chain::Destructor,
    Chain::PostConstruct,
];

/// Head addresses of the four chains of one struct. Zero is an empty chain.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct LinkHeads {
    property: u64,
    reference: u64,
    destructor: u64,
    post_construct: u64,
}

impl Chain {
    /// `true` when the chain continues into the parent's chain. ASSUMED: the
    /// property and reference chains do. The destructor and post-construct
    /// chains cover the properties of the struct itself, so their head is
    /// always one of its own properties, as on the measured 4.21 build.
    fn includes_inherited(self) -> bool {
        matches!(self, Self::Property | Self::Ref)
    }

    fn next_offset(self, links: &PropertyLinks) -> u32 {
        match self {
            Self::Property => links.property_next,
            Self::Ref => links.ref_next,
            Self::Destructor => links.destructor_next,
            Self::PostConstruct => links.post_construct_next,
        }
    }
}

impl LinkHeads {
    fn set(&mut self, chain: Chain, head: u64) {
        match chain {
            Chain::Property => self.property = head,
            Chain::Ref => self.reference = head,
            Chain::Destructor => self.destructor = head,
            Chain::PostConstruct => self.post_construct = head,
        }
    }

    /// Writes the four heads into the struct at `addr`.
    pub(crate) fn write(self, writer: &mut Writer<'_>, addr: u64) {
        let offsets: StructLinks = writer.p.shape.struct_links.clone();
        let mem = &mut *writer.mem;
        mem.put_u64(addr + u64::from(offsets.property_link), self.property);
        mem.put_u64(addr + u64::from(offsets.ref_link), self.reference);
        mem.put_u64(addr + u64::from(offsets.destructor_link), self.destructor);
        mem.put_u64(
            addr + u64::from(offsets.post_construct_link),
            self.post_construct,
        );
    }
}

impl Writer<'_> {
    fn struct_def(&self, path: &str) -> Option<&StructDef> {
        self.schema.structs.iter().find(|def| def.path == path)
    }

    /// `true` when a member of the struct at `path`, or of one of its
    /// parents, satisfies `test`.
    fn any_struct_member(
        &self,
        path: &str,
        depth: usize,
        test: fn(&Self, &PropertyType, usize) -> bool,
    ) -> bool {
        let mut current = Some(path);
        let mut parents = 0;
        while let Some(def) = current.and_then(|p| self.struct_def(p)) {
            if def
                .fields
                .iter()
                .any(|field| test(self, &field.ty, depth + 1))
            {
                return true;
            }
            parents += 1;
            if parents > MAX_PARENTS {
                break;
            }
            current = def.super_path.as_deref();
        }
        false
    }

    /// `ContainsObjectReference`. ASSUMED: hard, weak and lazy pointers,
    /// classes and interfaces, alone or inside containers and structs. Soft
    /// pointers hold a path, not an object, and are left out.
    fn holds_object_reference(&self, ty: &PropertyType, depth: usize) -> bool {
        if depth > MAX_NESTING {
            return false;
        }
        match ty {
            PropertyType::Object { .. }
            | PropertyType::Class { .. }
            | PropertyType::WeakObject { .. }
            | PropertyType::LazyObject { .. }
            | PropertyType::Interface { .. } => true,
            PropertyType::Array { inner: element }
            | PropertyType::Set { element }
            | PropertyType::Optional { inner: element } => {
                self.holds_object_reference(element, depth + 1)
            }
            PropertyType::Map { key, value } => {
                self.holds_object_reference(key, depth + 1)
                    || self.holds_object_reference(value, depth + 1)
            }
            PropertyType::Struct { path } => {
                self.any_struct_member(path, depth, Self::holds_object_reference)
            }
            _ => false,
        }
    }

    /// Needs a destructor call. ASSUMED: strings, text, containers,
    /// delegates, soft pointers, and structs that contain any of them.
    fn needs_destruction(&self, ty: &PropertyType, depth: usize) -> bool {
        if depth > MAX_NESTING {
            return false;
        }
        match ty {
            PropertyType::Str
            | PropertyType::Text
            | PropertyType::Array { .. }
            | PropertyType::Set { .. }
            | PropertyType::Map { .. }
            | PropertyType::Delegate { .. }
            | PropertyType::MulticastInlineDelegate { .. }
            | PropertyType::MulticastSparseDelegate { .. }
            | PropertyType::SoftObject { .. }
            | PropertyType::SoftClass { .. } => true,
            PropertyType::Optional { inner } => self.needs_destruction(inner, depth + 1),
            PropertyType::Struct { path } => {
                self.any_struct_member(path, depth, Self::needs_destruction)
            }
            _ => false,
        }
    }

    fn in_chain(&self, chain: Chain, node: usize) -> bool {
        let Some(node) = self.nodes.get(node) else {
            return false;
        };
        match chain {
            Chain::Property => true,
            Chain::Ref => self.holds_object_reference(&node.ty, 0),
            Chain::Destructor => self.needs_destruction(&node.ty, 0),
            Chain::PostConstruct => node.flags & (CPF_CONFIG | CPF_INSTANCED_REFERENCE) != 0,
        }
    }

    /// The top-level properties of `owner` followed by those of its parents,
    /// nearest parent first.
    fn properties_with_inherited(&self, owner: &str) -> Vec<usize> {
        let mut out = Vec::new();
        let mut current = Some(owner.to_owned());
        let mut steps = 0;
        while let Some(path) = current {
            if let Some(own) = self.chains.get(&path) {
                out.extend(own.iter().copied());
            }
            steps += 1;
            if steps > MAX_PARENTS {
                break;
            }
            current = self.super_of.get(&path).cloned();
        }
        out
    }

    /// Threads the four link chains through the properties and records the
    /// head of each chain per struct and function.
    pub(crate) fn compute_links(&mut self) {
        let owners: Vec<String> = self
            .objs
            .iter()
            .filter(|o| matches!(o.kind, Kind::Struct(_) | Kind::Function { .. }))
            .map(|o| o.path.clone())
            .collect();
        let offsets = self.p.shape.property_links.clone();
        for owner in owners {
            let all = self.properties_with_inherited(&owner);
            let own = self.chains.get(&owner).cloned().unwrap_or_default();
            let mut heads = LinkHeads::default();
            for chain in CHAINS {
                let source = if chain.includes_inherited() {
                    &all
                } else {
                    &own
                };
                let members: Vec<u64> = source
                    .iter()
                    .filter(|&&node| self.in_chain(chain, node))
                    .filter_map(|&node| self.nodes.get(node).map(|n| n.addr))
                    .collect();
                // A parent's members get the same pointers when its own
                // chain is threaded, so rewriting them here is harmless.
                let next = u64::from(chain.next_offset(&offsets));
                for pair in members.windows(2) {
                    if let [a, b] = pair {
                        self.mem.put_u64(a + next, *b);
                    }
                }
                heads.set(chain, members.first().copied().unwrap_or(0));
            }
            self.link_heads.insert(owner, heads);
        }
    }
}
