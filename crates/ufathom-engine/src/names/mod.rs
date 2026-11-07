//! The name table: every `FName` index mapped to its text.
//!
//! [`NameTable::load`] bulk-reads either design of the engine's name storage
//! once. After that, resolving a name needs no target memory, so a lookup
//! never faults and never blocks.

mod array;
mod decode;
mod pool;

use rayon::ThreadPool;
use ufathom_mem::{Addr, MemoryReader};
use ufathom_model::EngineLayout;
use ufathom_model::layout::{FNameLayout, NameTableLayout};

use crate::error::EngineError;
use crate::limits::{MAX_NAME_ENTRIES, MAX_NAME_TABLE_BYTES};

/// How [`NameTable::load`] runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct LoadOptions<'a> {
    /// Thread pool for reading blocks in parallel. `None` reads sequentially.
    ///
    /// The global rayon pool is never used: its threads cannot be stopped,
    /// and the library must be able to unload. Every parallel iterator runs
    /// inside [`ThreadPool::install`] on this pool.
    pub pool: Option<&'a ThreadPool>,
    /// The build keeps `FName` numbers outside the `FName`. A pool entry with
    /// length zero then is an indirection to another entry plus a number,
    /// not the end of a block.
    pub outline_numbers: bool,
}

impl<'a> LoadOptions<'a> {
    /// Options matching `layout`: outline numbers when the `FName` has no
    /// number field.
    #[must_use]
    pub fn for_layout(layout: &EngineLayout, pool: Option<&'a ThreadPool>) -> Self {
        Self {
            pool,
            outline_numbers: layout.fname.number.is_none(),
        }
    }
}

/// A block or chunk that did not load completely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockProblem {
    /// Position of the block (pool) or chunk (array), from zero.
    pub block: u32,
    /// Address of the block, or zero when its pointer was null.
    pub addr: Addr,
    /// What went wrong.
    pub reason: String,
}

/// What a load saw, including everything that was skipped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoadStats {
    /// Blocks (pool) or chunks (array) the table claims to use.
    pub blocks: u32,
    /// Blocks after the first that could not be read. Their names resolve to
    /// `None`.
    pub unreadable_blocks: Vec<BlockProblem>,
    /// Blocks where a malformed entry ended the walk: the names before it are
    /// kept, the rest of the block is lost.
    pub damaged_blocks: Vec<BlockProblem>,
    /// Entries recorded, resolvable or not.
    pub entries: usize,
    /// Array entries whose pointer was set but could not be read.
    pub unreadable_entries: u32,
    /// Outline-number entries whose target entry is missing or is itself an
    /// indirection.
    pub unresolved_numbered: u32,
}

/// One block's worth of decoded entries.
#[derive(Debug, Default)]
pub(crate) struct ParsedBlock {
    pub(crate) entries: Vec<(u32, Parsed)>,
    pub(crate) damaged: Option<String>,
    pub(crate) unreadable: u32,
}

/// One decoded entry.
#[derive(Debug)]
pub(crate) enum Parsed {
    Text(String),
    /// The entry names another entry and a number (outline numbers).
    Numbered {
        id: u32,
        number: u32,
    },
}

/// The outcome of reading one block.
#[derive(Debug)]
pub(crate) struct BlockResult {
    pub(crate) block: u32,
    pub(crate) addr: Addr,
    pub(crate) outcome: Result<ParsedBlock, EngineError>,
}

#[derive(Debug, Clone, Copy)]
struct Entry {
    index: u32,
    start: u32,
    end: u32,
    has_text: bool,
}

/// Every name of the target, by index.
#[derive(Debug, Default)]
pub struct NameTable {
    /// Sorted by `index`.
    entries: Vec<Entry>,
    text: String,
    stats: LoadStats,
}

impl NameTable {
    /// Reads the whole name table described by `layout`.
    ///
    /// Pool mode reads the block pointers and each block, walking entries and
    /// using the current block and cursor for the end of the last block. Array
    /// mode reads chunk pointers, then entry pointers, then entries. With
    /// `options.pool` set, blocks are read in parallel on that pool. The
    /// result does not depend on whether a pool was used.
    ///
    /// A block that cannot be read is an error only if it is the first one.
    /// Later ones are listed in [`LoadStats`] and their names resolve to
    /// `None`.
    ///
    /// # Errors
    ///
    /// The layout is unusable, the table header or block pointers cannot be
    /// read, a count is above its limit, or the first block cannot be read.
    pub fn load(
        mem: &dyn MemoryReader,
        layout: &NameTableLayout,
        options: &LoadOptions<'_>,
    ) -> Result<Self, EngineError> {
        let results = match layout {
            NameTableLayout::Pool(pool_layout) => pool::load(mem, pool_layout, options)?,
            NameTableLayout::Array(array_layout) => array::load(mem, array_layout, options)?,
        };
        Self::assemble(results)
    }

    fn assemble(results: Vec<BlockResult>) -> Result<Self, EngineError> {
        let mut stats = LoadStats {
            blocks: results.len() as u32,
            ..LoadStats::default()
        };
        let mut entries: Vec<Entry> = Vec::new();
        let mut text = String::new();
        let mut numbered: Vec<(u32, u32, u32)> = Vec::new();
        for (position, result) in results.into_iter().enumerate() {
            let parsed = match result.outcome {
                Ok(parsed) => parsed,
                Err(error) if position == 0 => return Err(error),
                Err(error) => {
                    tracing::warn!(block = result.block, addr = result.addr, %error, "name block unreadable");
                    stats.unreadable_blocks.push(BlockProblem {
                        block: result.block,
                        addr: result.addr,
                        reason: error.to_string(),
                    });
                    continue;
                }
            };
            if let Some(reason) = parsed.damaged {
                tracing::warn!(block = result.block, %reason, "name block damaged");
                stats.damaged_blocks.push(BlockProblem {
                    block: result.block,
                    addr: result.addr,
                    reason,
                });
            }
            stats.unreadable_entries += parsed.unreadable;
            for (index, entry) in parsed.entries {
                if entries.len() as u64 >= MAX_NAME_ENTRIES {
                    return Err(EngineError::Limit {
                        addr: result.addr,
                        what: "name entries",
                        value: entries.len() as u64 + 1,
                        limit: MAX_NAME_ENTRIES,
                    });
                }
                match entry {
                    Parsed::Text(name) => {
                        let start = text.len();
                        if start + name.len() > MAX_NAME_TABLE_BYTES {
                            return Err(EngineError::Limit {
                                addr: result.addr,
                                what: "name text bytes",
                                value: (start + name.len()) as u64,
                                limit: MAX_NAME_TABLE_BYTES as u64,
                            });
                        }
                        text.push_str(&name);
                        entries.push(Entry {
                            index,
                            start: start as u32,
                            end: text.len() as u32,
                            has_text: true,
                        });
                    }
                    Parsed::Numbered { id, number } => {
                        numbered.push((index, id, number));
                        entries.push(Entry {
                            index,
                            start: 0,
                            end: 0,
                            has_text: false,
                        });
                    }
                }
            }
        }
        if !entries.windows(2).all(|pair| match pair {
            [a, b] => a.index < b.index,
            _ => true,
        }) {
            entries.sort_by_key(|entry| entry.index);
        }
        let mut table = Self {
            entries,
            text,
            stats,
        };
        table.resolve_numbered(&numbered)?;
        table.stats.entries = table.entries.len();
        tracing::debug!(
            entries = table.stats.entries,
            blocks = table.stats.blocks,
            unreadable_blocks = table.stats.unreadable_blocks.len(),
            "name table loaded"
        );
        Ok(table)
    }

    /// Gives each outline-number entry the text of its target plus `_{N-1}`.
    fn resolve_numbered(&mut self, numbered: &[(u32, u32, u32)]) -> Result<(), EngineError> {
        for &(index, id, number) in numbered {
            let base = self
                .position_of(id)
                .and_then(|target| self.entries.get(target))
                .filter(|target| target.has_text)
                .and_then(|target| self.text.get(target.start as usize..target.end as usize))
                .map(str::to_owned);
            let Some(base) = base else {
                self.stats.unresolved_numbered += 1;
                continue;
            };
            let name = format!("{base}_{}", number - 1);
            let start = self.text.len();
            if start + name.len() > MAX_NAME_TABLE_BYTES {
                return Err(EngineError::Limit {
                    addr: 0,
                    what: "name text bytes",
                    value: (start + name.len()) as u64,
                    limit: MAX_NAME_TABLE_BYTES as u64,
                });
            }
            self.text.push_str(&name);
            let position = self.position_of(index);
            if let Some(entry) = position.and_then(|at| self.entries.get_mut(at)) {
                entry.start = start as u32;
                entry.end = self.text.len() as u32;
                entry.has_text = true;
            }
        }
        Ok(())
    }

    fn position_of(&self, index: u32) -> Option<usize> {
        self.entries
            .binary_search_by_key(&index, |entry| entry.index)
            .ok()
    }

    /// What the load saw.
    #[must_use]
    pub fn stats(&self) -> &LoadStats {
        &self.stats
    }

    /// Number of entries that have text.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.iter().filter(|entry| entry.has_text).count()
    }

    /// Whether no entry has text.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.entries.iter().any(|entry| entry.has_text)
    }

    /// The text of the entry with this index. `None` for an index that is not
    /// an entry, including one far outside the table.
    #[must_use]
    pub fn resolve_index(&self, index: u32) -> Option<&str> {
        let entry = self.entries.get(self.position_of(index)?)?;
        if !entry.has_text {
            return None;
        }
        self.text.get(entry.start as usize..entry.end as usize)
    }

    /// Whether `index` is the start of an entry seen during the load.
    ///
    /// For a pool this is how a solver confirms the number of block offset
    /// bits: a wrong value sends real names to indices that are not starts.
    #[must_use]
    pub fn is_entry_start(&self, index: u32) -> bool {
        self.position_of(index).is_some()
    }

    /// Every entry that has text, in index order.
    pub fn iter(&self) -> impl Iterator<Item = (u32, &str)> + '_ {
        self.entries.iter().filter_map(|entry| {
            if !entry.has_text {
                return None;
            }
            let text = self.text.get(entry.start as usize..entry.end as usize)?;
            Some((entry.index, text))
        })
    }

    /// Resolves the `FName` stored in `name_bytes`.
    ///
    /// The text comes from the display index when `fname` has one, falling
    /// back to the comparison index when the display index does not resolve.
    /// A number above zero is appended as `_{Number-1}`.
    ///
    /// # Errors
    ///
    /// [`EngineError::Malformed`] when `name_bytes` is too short for the
    /// layout, [`EngineError::UnresolvedName`] when no index resolves.
    pub fn resolve(&self, fname: &FNameLayout, name_bytes: &[u8]) -> Result<String, EngineError> {
        let comparison = field_u32(name_bytes, fname.comparison_index)?;
        let display = fname
            .display_index
            .map(|offset| field_u32(name_bytes, offset))
            .transpose()?;
        let number = fname
            .number
            .map(|offset| field_u32(name_bytes, offset))
            .transpose()?
            .unwrap_or(0);
        let base = display
            .and_then(|index| self.resolve_index(index))
            .or_else(|| self.resolve_index(comparison))
            .ok_or(EngineError::UnresolvedName {
                index: display.unwrap_or(comparison),
            })?;
        Ok(if number == 0 {
            base.to_owned()
        } else {
            format!("{base}_{}", number - 1)
        })
    }
}

fn field_u32(bytes: &[u8], offset: u32) -> Result<u32, EngineError> {
    let at = offset as usize;
    bytes
        .get(at..at + 4)
        .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        .map(u32::from_le_bytes)
        .ok_or_else(|| EngineError::Malformed {
            what: "FName bytes",
            reason: format!("{} bytes cannot hold a field at {offset:#x}", bytes.len()),
        })
}
