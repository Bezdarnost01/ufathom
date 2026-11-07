//! Bounded walks over singly linked member lists in target memory.

use std::collections::HashSet;

use ufathom_mem::{Addr, MemoryReader, MemoryReaderExt};

use crate::error::EngineError;
use crate::limits::{MAX_MEMBER_CHAIN_LENGTH, MAX_PROPERTIES_PER_STRUCT};

/// Decides whether a node belongs in the output. An error is yielded as an
/// item and the walk goes on, since the list itself is intact.
type Accept<'a> = Box<dyn Fn(Addr) -> Result<bool, EngineError> + Send + Sync + 'a>;

/// The members of one owner, as addresses.
///
/// Every item is a node address or the reason the walk cannot go on. The
/// walk reads a node's `Next` link before yielding the node, so an address
/// that cannot be read is reported as an error item, never yielded. After an
/// error from the list itself (unreadable link, cycle, limit) the iterator
/// ends. A rejected node (filter error) does not end it.
///
/// Limits: [`MAX_MEMBER_CHAIN_LENGTH`] links walked and
/// [`MAX_PROPERTIES_PER_STRUCT`] nodes yielded.
pub struct FieldIter<'a> {
    mem: &'a dyn MemoryReader,
    what: &'static str,
    start: Option<Result<(Addr, u32), EngineError>>,
    next_offset: u32,
    cursor: Option<Addr>,
    visited: HashSet<Addr>,
    yielded: usize,
    accept: Accept<'a>,
}

impl std::fmt::Debug for FieldIter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FieldIter")
            .field("what", &self.what)
            .field("cursor", &self.cursor)
            .field("yielded", &self.yielded)
            .finish_non_exhaustive()
    }
}

impl<'a> FieldIter<'a> {
    /// A walk of the list whose head pointer sits at `owner + head_offset`.
    pub(crate) fn walk(
        mem: &'a dyn MemoryReader,
        what: &'static str,
        owner: Addr,
        head_offset: u32,
        next_offset: u32,
        accept: Accept<'a>,
    ) -> Self {
        Self::with_start(mem, what, Ok((owner, head_offset)), next_offset, accept)
    }

    /// An iterator that yields `error` once and ends.
    pub(crate) fn failed(
        mem: &'a dyn MemoryReader,
        what: &'static str,
        error: EngineError,
    ) -> Self {
        Self::with_start(mem, what, Err(error), 0, Box::new(|_| Ok(false)))
    }

    fn with_start(
        mem: &'a dyn MemoryReader,
        what: &'static str,
        start: Result<(Addr, u32), EngineError>,
        next_offset: u32,
        accept: Accept<'a>,
    ) -> Self {
        Self {
            mem,
            what,
            start: Some(start),
            next_offset,
            cursor: None,
            visited: HashSet::new(),
            yielded: 0,
            accept,
        }
    }

    fn begin(&mut self) -> Option<EngineError> {
        let start = self.start.take()?;
        match start {
            Err(error) => Some(error),
            Ok((owner, head_offset)) => {
                let at = owner.wrapping_add(u64::from(head_offset));
                match self.mem.read_u64(at) {
                    Ok(head) => {
                        self.cursor = Some(head);
                        None
                    }
                    Err(source) => Some(EngineError::Read {
                        addr: at,
                        what: self.what,
                        source,
                    }),
                }
            }
        }
    }
}

impl Iterator for FieldIter<'_> {
    type Item = Result<Addr, EngineError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.start.is_some()
            && let Some(error) = self.begin()
        {
            return Some(Err(error));
        }
        loop {
            let current = self.cursor?;
            if current == 0 {
                self.cursor = None;
                return None;
            }
            if !self.visited.insert(current) {
                self.cursor = None;
                return Some(Err(EngineError::Cycle {
                    addr: current,
                    what: self.what,
                }));
            }
            if self.visited.len() > MAX_MEMBER_CHAIN_LENGTH {
                self.cursor = None;
                return Some(Err(EngineError::Limit {
                    addr: current,
                    what: "member chain length",
                    value: self.visited.len() as u64,
                    limit: MAX_MEMBER_CHAIN_LENGTH as u64,
                }));
            }
            let at = current.wrapping_add(u64::from(self.next_offset));
            let next = match self.mem.read_u64(at) {
                Ok(next) => next,
                Err(source) => {
                    self.cursor = None;
                    return Some(Err(EngineError::Read {
                        addr: at,
                        what: self.what,
                        source,
                    }));
                }
            };
            self.cursor = Some(next);
            match (self.accept)(current) {
                Ok(false) => {}
                Ok(true) => {
                    if self.yielded >= MAX_PROPERTIES_PER_STRUCT {
                        self.cursor = None;
                        return Some(Err(EngineError::Limit {
                            addr: current,
                            what: "members of one struct",
                            value: self.yielded as u64 + 1,
                            limit: MAX_PROPERTIES_PER_STRUCT as u64,
                        }));
                    }
                    self.yielded += 1;
                    return Some(Ok(current));
                }
                Err(error) => return Some(Err(error)),
            }
        }
    }
}
