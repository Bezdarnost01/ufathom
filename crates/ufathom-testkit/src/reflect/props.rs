//! Properties: `UProperty` objects (legacy) or `FField` chains (modern).

use ufathom_model::layout::PropertySystem;
use ufathom_model::{FieldDef, PropertyType};

use crate::schema::{
    CPF_ZERO_CONSTRUCTOR, PROPERTY_CLASSES, payload_size, property_class_exists,
    property_class_name, type_layout,
};
use crate::space::Pool;

use super::{FLAGS_MEMBER, Kind, Writer};

/// A property to be written, with its nested inner properties.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub path: String,
    pub name: String,
    pub ty: PropertyType,
    pub class_name: String,
    pub offset: u32,
    pub array_dim: u32,
    pub element_size: u32,
    pub flags: u64,
    /// Path of the owning struct or function, or of the parent property.
    pub owner: String,
    pub parent: Option<usize>,
    pub inner: Vec<usize>,
    pub addr: u64,
    pub obj: Option<usize>,
}

impl Writer<'_> {
    /// Plans one top-level property of `owner` and its inner properties.
    pub(crate) fn plan_property(&mut self, owner: &str, field: &FieldDef) {
        let idx = self.plan_node(
            owner,
            None,
            &field.name,
            field.ty.clone(),
            field.offset,
            field.array_dim,
            field.element_size,
            field.flags,
        );
        self.chains.entry(owner.to_owned()).or_default().push(idx);
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "mirrors the fields of a property"
    )]
    fn plan_node(
        &mut self,
        owner: &str,
        parent: Option<usize>,
        name: &str,
        ty: PropertyType,
        offset: u32,
        array_dim: u32,
        element_size: u32,
        flags: u64,
    ) -> usize {
        assert!(
            !matches!(ty, PropertyType::Optional { .. }),
            "the testkit does not write optional properties"
        );
        let class_name = property_class_name(&ty, self.p);
        let path = format!("{owner}:{name}");
        let size = self.p.layout.properties.size + payload_size(&ty);
        let idx = self.nodes.len();
        let mut node = Node {
            path: path.clone(),
            name: name.to_owned(),
            ty: ty.clone(),
            class_name: class_name.clone(),
            offset,
            array_dim,
            element_size,
            flags,
            owner: owner.to_owned(),
            parent,
            inner: Vec::new(),
            addr: 0,
            obj: None,
        };
        if self.p.is_modern() {
            node.addr = self
                .mem
                .heap
                .alloc(Pool::Objects, usize::try_from(size).unwrap_or(0), 16);
            self.nodes.push(node);
        } else {
            let class = Self::class_path(&class_name);
            let obj = self.add_obj(
                &path,
                &class,
                Some(owner),
                name,
                0,
                size,
                FLAGS_MEMBER,
                Kind::Property,
            );
            node.addr = self.objs.get(obj).map_or(0, |o| o.addr);
            node.obj = Some(obj);
            self.nodes.push(node);
        }
        self.node_by_path.insert(path.clone(), idx);

        let children: Vec<(PropertyType, String)> = match ty {
            PropertyType::Array { inner } => vec![(*inner, name.to_owned())],
            PropertyType::Set { element } => vec![(*element, name.to_owned())],
            PropertyType::Map { key, value } => {
                vec![(*key, format!("{name}_Key")), (*value, name.to_owned())]
            }
            PropertyType::Enum { underlying, .. } => {
                vec![(*underlying, "UnderlyingType".to_owned())]
            }
            _ => Vec::new(),
        };
        for (child_ty, child_name) in children {
            let (child_size, _) = type_layout(&child_ty, self.p, &self.sizes);
            let child = self.plan_node(
                &path,
                Some(idx),
                &child_name,
                child_ty,
                0,
                1,
                child_size,
                CPF_ZERO_CONSTRUCTOR,
            );
            if let Some(node) = self.nodes.get_mut(idx) {
                node.inner.push(child);
            }
        }
        idx
    }

    /// Allocates one `FFieldClass` per property class (modern builds).
    pub(crate) fn plan_field_classes(&mut self) {
        let Some(shape) = self.p.shape.field_class.as_ref() else {
            return;
        };
        let size = usize::try_from(shape.size).unwrap_or(0x48);
        for &(name, _) in PROPERTY_CLASSES {
            if property_class_exists(name, self.p) {
                let addr = self.mem.heap.alloc(Pool::Objects, size, 16);
                self.field_classes.insert(name.to_owned(), addr);
            }
        }
    }

    /// Writes the `FFieldClass` objects.
    pub(crate) fn write_field_classes(&mut self) {
        let PropertySystem::Modern { field_class, .. } = self.p.layout.properties.system.clone()
        else {
            return;
        };
        let Some(shape) = self.p.shape.field_class.clone() else {
            return;
        };
        let classes: Vec<(String, u64)> = self
            .field_classes
            .iter()
            .map(|(name, &addr)| (name.clone(), addr))
            .collect();
        for (name, addr) in classes {
            self.write_fname(addr + u64::from(field_class.name), &name, 0);
            let cast = self.property_cast_flags(&name);
            let own = self.own_cast_bit(&name);
            self.mem.put_u64(addr + u64::from(shape.id), own);
            if let Some(cast_offset) = field_class.cast_flags {
                self.mem.put_u64(addr + u64::from(cast_offset), cast);
            }
            let parent = PROPERTY_CLASSES
                .iter()
                .find(|&&(class, _)| class == name)
                .map(|&(_, parent)| parent);
            if let Some(parent_addr) = parent.and_then(|p| self.field_classes.get(p)).copied() {
                self.mem
                    .put_u64(addr + u64::from(shape.super_class), parent_addr);
            }
            let thunk = self.code.native_thunk(self.mem);
            self.mem
                .put_u64(addr + u64::from(shape.construct_fn), thunk);
        }
    }

    /// Cast flags of a property class: its own bit plus its ancestors'.
    fn property_cast_flags(&self, name: &str) -> u64 {
        let mut flags = 0;
        let mut current = Some(name);
        while let Some(class) = current {
            flags |= self.own_cast_bit(class);
            current = PROPERTY_CLASSES
                .iter()
                .find(|&&(candidate, _)| candidate == class)
                .map(|&(_, parent)| parent)
                .filter(|&parent| parent != "Field");
        }
        flags
    }

    /// Writes the body (and, for modern builds, the header) of every property.
    pub(crate) fn write_properties(&mut self) {
        self.write_field_classes();
        for index in 0..self.nodes.len() {
            let Some(node) = self.nodes.get(index).cloned() else {
                continue;
            };
            if self.p.is_modern() {
                self.write_ffield_header(&node);
            }
            self.write_property_body(&node);
        }
    }

    fn write_ffield_header(&mut self, node: &Node) {
        let PropertySystem::Modern { ffield, .. } = self.p.layout.properties.system.clone() else {
            return;
        };
        let Some(shape) = self.p.shape.ffield.clone() else {
            return;
        };
        let key = format!("FField:{}", node.class_name);
        let vtable = self.vtable(&key);
        let addr = node.addr;
        self.mem.put_u64(addr, vtable);
        let class = self
            .field_classes
            .get(&node.class_name)
            .copied()
            .unwrap_or(0);
        self.mem.put_u64(addr + u64::from(ffield.class), class);
        let (owner, is_uobject) = match node.parent.and_then(|p| self.nodes.get(p)) {
            Some(parent) => (parent.addr, 0),
            None => (self.addr_of(&node.owner), 1),
        };
        self.mem.put_u64(addr + u64::from(shape.owner), owner);
        if let Some(offset) = shape.owner_is_uobject {
            self.mem.put_u8(addr + u64::from(offset), is_uobject);
        }
        self.write_fname(addr + u64::from(ffield.name), &node.name, 0);
        if let Some(flags) = ffield.flags {
            self.mem.put_u32(addr + u64::from(flags), FLAGS_MEMBER);
        }
    }

    fn write_property_body(&mut self, node: &Node) {
        let props = self.p.layout.properties.clone();
        let a = node.addr;
        self.mem
            .put_u32(a + u64::from(props.array_dim), node.array_dim);
        self.mem
            .put_u32(a + u64::from(props.element_size), node.element_size);
        self.mem.put_u64(a + u64::from(props.flags), node.flags);
        self.mem
            .put_u32(a + u64::from(props.offset_internal), node.offset);

        let typed = &props.typed;
        let put_ptr = |this: &mut Self, offset: Option<u32>, target: u64| {
            if let Some(offset) = offset {
                this.mem.put_u64(a + u64::from(offset), target);
            }
        };
        let inner = |index: usize| {
            node.inner
                .get(index)
                .and_then(|&n| self.nodes.get(n))
                .map_or(0, |n| n.addr)
        };
        let inner0 = inner(0);
        let inner1 = inner(1);
        match &node.ty {
            PropertyType::Bool {
                byte_offset,
                byte_mask,
                field_mask,
                field_size,
            } => {
                if let Some(bool_layout) = &typed.bool_property {
                    self.mem
                        .put_u8(a + u64::from(bool_layout.field_size), *field_size);
                    self.mem
                        .put_u8(a + u64::from(bool_layout.byte_offset), *byte_offset);
                    self.mem
                        .put_u8(a + u64::from(bool_layout.byte_mask), *byte_mask);
                    self.mem
                        .put_u8(a + u64::from(bool_layout.field_mask), *field_mask);
                }
            }
            PropertyType::Object { class }
            | PropertyType::WeakObject { class }
            | PropertyType::LazyObject { class }
            | PropertyType::SoftObject { class } => {
                let target = self.addr_of(class);
                put_ptr(self, typed.object_class, target);
            }
            PropertyType::Class { class, meta } => {
                let class = self.addr_of(class);
                let meta = self.addr_of(meta);
                put_ptr(self, typed.object_class, class);
                put_ptr(self, typed.class_meta, meta);
            }
            PropertyType::SoftClass { meta } => {
                let class = self.addr_of(&Self::class_path("Class"));
                let meta = self.addr_of(meta);
                put_ptr(self, typed.object_class, class);
                put_ptr(self, typed.class_meta, meta);
            }
            PropertyType::Interface { class } => {
                let target = self.addr_of(class);
                put_ptr(self, typed.interface_class, target);
            }
            PropertyType::Struct { path } => {
                let target = self.addr_of(path);
                put_ptr(self, typed.script_struct, target);
            }
            PropertyType::Array { .. } => put_ptr(self, typed.array_inner, inner0),
            PropertyType::Set { .. } => put_ptr(self, typed.set_element, inner0),
            PropertyType::Map { .. } => {
                put_ptr(self, typed.map_key, inner0);
                put_ptr(self, typed.map_value, inner1);
            }
            PropertyType::Enum { path, .. } => {
                let target = self.addr_of(path);
                put_ptr(self, typed.enum_underlying, inner0);
                put_ptr(self, typed.enum_type, target);
            }
            PropertyType::Byte { path } => {
                let target = self.addr_of(path);
                put_ptr(self, typed.byte_enum, target);
            }
            PropertyType::Delegate { signature }
            | PropertyType::MulticastInlineDelegate { signature }
            | PropertyType::MulticastSparseDelegate { signature } => {
                let target = self.addr_of(signature);
                put_ptr(self, typed.delegate_signature, target);
            }
            _ => {}
        }
    }
}
