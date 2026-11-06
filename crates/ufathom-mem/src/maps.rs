//! Parser for `/proc/<pid>/maps`.
//!
//! Pure functions over text so they are tested on every platform. Only the
//! Linux backend calls them outside tests.

use std::collections::{BTreeMap, HashSet};

use crate::types::{MemoryRegion, ModuleInfo, Perms, RegionKind, file_name};

/// One line of a maps file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MapsEntry {
    pub(crate) start: u64,
    pub(crate) end: u64,
    pub(crate) perms: Perms,
    /// `s` in the permission column: shared rather than private.
    pub(crate) shared: bool,
    /// Pathname column, such as `/usr/lib/libc.so.6` or `[heap]`. Empty for anonymous memory.
    pub(crate) path: String,
}

impl MapsEntry {
    fn is_file_backed(&self) -> bool {
        self.path.starts_with('/')
    }
}

/// Parses maps text. Lines that do not follow the format are skipped, because
/// the content comes from a process that may change while it is read.
pub(crate) fn parse_maps(text: &str) -> Vec<MapsEntry> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<MapsEntry> {
    // address perms offset dev inode [pathname with possible spaces]
    let mut rest = line;
    let mut fields = [""; 5];
    for field in &mut fields {
        rest = rest.trim_start();
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        *field = rest.get(..end)?;
        rest = rest.get(end..)?;
    }
    let [range, perms, _offset, _dev, _inode] = fields;
    let (start, end) = range.split_once('-')?;
    let start = u64::from_str_radix(start, 16).ok()?;
    let end = u64::from_str_radix(end, 16).ok()?;
    if end < start {
        return None;
    }
    let mut chars = perms.chars();
    let (read, write, exec, sharing) = (chars.next()?, chars.next()?, chars.next()?, chars.next()?);
    Some(MapsEntry {
        start,
        end,
        perms: Perms::new(read == 'r', write == 'w', exec == 'x'),
        shared: sharing == 's',
        path: rest.trim().to_owned(),
    })
}

/// Paths that have at least one executable mapping.
fn executable_paths(entries: &[MapsEntry]) -> HashSet<&str> {
    entries
        .iter()
        .filter(|entry| entry.is_file_backed() && entry.perms.execute)
        .map(|entry| entry.path.as_str())
        .collect()
}

/// Converts entries to regions. File-backed memory is `Image` when the file is
/// also mapped executable somewhere, and `Mapped` otherwise.
pub(crate) fn regions_from_maps(entries: &[MapsEntry]) -> Vec<MemoryRegion> {
    let executable = executable_paths(entries);
    let mut regions: Vec<MemoryRegion> = entries
        .iter()
        .filter(|entry| entry.end > entry.start)
        .map(|entry| {
            let kind = if entry.is_file_backed() {
                if executable.contains(entry.path.as_str()) {
                    RegionKind::Image
                } else {
                    RegionKind::Mapped
                }
            } else if entry.path == "[vdso]" {
                RegionKind::Image
            } else if entry.path.is_empty()
                || entry.path == "[heap]"
                || entry.path.starts_with("[stack")
                || entry.path.starts_with("[anon")
            {
                if entry.shared {
                    RegionKind::Mapped
                } else {
                    RegionKind::Private
                }
            } else {
                RegionKind::Unknown
            };
            MemoryRegion {
                base: entry.start,
                size: entry.end - entry.start,
                perms: entry.perms,
                kind,
            }
        })
        .collect();
    regions.sort_by_key(|region| region.base);
    regions
}

/// Whether a path names a file that can plausibly be a loaded image.
fn is_module_path(path: &str) -> bool {
    path.starts_with('/')
        && !path.starts_with("/dev/")
        && !path.starts_with("/memfd:")
        && !path.starts_with("/SYSV")
}

/// Derives modules from file-backed mappings. Mappings of one file are merged
/// into one module spanning the lowest start to the highest end. A file with no
/// executable mapping is data, not a module.
pub(crate) fn modules_from_maps(entries: &[MapsEntry]) -> Vec<ModuleInfo> {
    let executable = executable_paths(entries);
    let mut spans: BTreeMap<&str, (u64, u64)> = BTreeMap::new();
    for entry in entries {
        if !is_module_path(&entry.path) || !executable.contains(entry.path.as_str()) {
            continue;
        }
        let span = spans
            .entry(entry.path.as_str())
            .or_insert((entry.start, entry.end));
        span.0 = span.0.min(entry.start);
        span.1 = span.1.max(entry.end);
    }
    let mut modules: Vec<ModuleInfo> = spans
        .into_iter()
        .map(|(path, (start, end))| {
            let clean = path.strip_suffix(" (deleted)").unwrap_or(path);
            ModuleInfo {
                name: file_name(clean).to_owned(),
                path: Some(path.to_owned()),
                base: start,
                size: end - start,
            }
        })
        .collect();
    modules.sort_by_key(|module| module.base);
    modules
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
55d0c8a00000-55d0c8a02000 r--p 00000000 08:01 1310725 /usr/bin/cat
55d0c8a02000-55d0c8a07000 r-xp 00002000 08:01 1310725 /usr/bin/cat
55d0c8a07000-55d0c8a0a000 r--p 00007000 08:01 1310725 /usr/bin/cat
55d0c8a0b000-55d0c8a0c000 rw-p 0000a000 08:01 1310725 /usr/bin/cat
55d0c9f2c000-55d0c9f4d000 rw-p 00000000 00:00 0 [heap]
7f2a10000000-7f2a10021000 rw-p 00000000 00:00 0
7f2a14000000-7f2a14001000 rw-s 00000000 00:05 9 /dev/shm/some shared file (deleted)
7f2a14a00000-7f2a14a20000 r--p 00000000 08:01 22 /usr/lib/locale/locale-archive
7f2a14c00000-7f2a14c28000 r--p 00000000 08:01 33 /usr/lib/x86_64-linux-gnu/libc.so.6
7f2a14c28000-7f2a14dbd000 r-xp 00028000 08:01 33 /usr/lib/x86_64-linux-gnu/libc.so.6
7f2a14dbd000-7f2a14e15000 r--p 001bd000 08:01 33 /usr/lib/x86_64-linux-gnu/libc.so.6
7f2a14e15000-7f2a14e19000 ---p 00215000 08:01 33 /usr/lib/x86_64-linux-gnu/libc.so.6
7ffd4a1c0000-7ffd4a1e1000 rw-p 00000000 00:00 0 [stack]
7ffd4a1f0000-7ffd4a1f4000 r--p 00000000 00:00 0 [vvar]
7ffd4a1f4000-7ffd4a1f6000 r-xp 00000000 00:00 0 [vdso]
ffffffffff600000-ffffffffff601000 --xp 00000000 00:00 0 [vsyscall]
";

    #[test]
    fn parses_every_well_formed_line() {
        let entries = parse_maps(SAMPLE);
        assert_eq!(entries.len(), 16);
        assert_eq!(entries[0].start, 0x55d0c8a00000);
        assert_eq!(entries[0].end, 0x55d0c8a02000);
        assert_eq!(entries[0].perms, Perms::R);
        assert_eq!(entries[0].path, "/usr/bin/cat");
        assert_eq!(entries[1].perms, Perms::RX);
        assert!(entries[5].path.is_empty());
        assert_eq!(entries[15].start, 0xffff_ffff_ff60_0000);
        assert_eq!(entries[15].perms, Perms::new(false, false, true));
    }

    #[test]
    fn paths_may_contain_spaces() {
        let entries = parse_maps(SAMPLE);
        assert_eq!(entries[6].path, "/dev/shm/some shared file (deleted)");
        assert!(entries[6].shared);
    }

    #[test]
    fn malformed_lines_are_skipped_not_fatal() {
        let text = "garbage\n\
                    zz-10 r--p 0 0:0 0\n\
                    20-10 r--p 0 0:0 0\n\
                    1000-2000 r-\n\
                    \n\
                    1000-2000 r--p 00000000 00:00 0 [heap]\n";
        let entries = parse_maps(text);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, "[heap]");
        assert!(parse_maps("").is_empty());
    }

    #[test]
    fn regions_get_kinds() {
        let regions = regions_from_maps(&parse_maps(SAMPLE));
        let kind_at = |base: u64| {
            regions
                .iter()
                .find(|r| r.base == base)
                .map(|r| r.kind)
                .unwrap()
        };
        assert_eq!(kind_at(0x55d0c8a00000), RegionKind::Image);
        assert_eq!(kind_at(0x55d0c9f2c000), RegionKind::Private);
        assert_eq!(kind_at(0x7f2a10000000), RegionKind::Private);
        assert_eq!(kind_at(0x7f2a14000000), RegionKind::Mapped);
        assert_eq!(kind_at(0x7f2a14a00000), RegionKind::Mapped);
        assert_eq!(kind_at(0x7ffd4a1c0000), RegionKind::Private);
        assert_eq!(kind_at(0x7ffd4a1f0000), RegionKind::Unknown);
        assert_eq!(kind_at(0x7ffd4a1f4000), RegionKind::Image);
        let guard = regions.iter().find(|r| r.base == 0x7f2a14e15000).unwrap();
        assert_eq!(guard.perms, Perms::NONE);
        assert_eq!(guard.size, 0x4000);
    }

    #[test]
    fn modules_merge_mappings_of_one_file() {
        let modules = modules_from_maps(&parse_maps(SAMPLE));
        let names: Vec<_> = modules.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["cat", "libc.so.6"]);
        let cat = &modules[0];
        assert_eq!(cat.base, 0x55d0c8a00000);
        assert_eq!(cat.size, 0x55d0c8a0c000 - 0x55d0c8a00000);
        assert_eq!(cat.path.as_deref(), Some("/usr/bin/cat"));
        let libc = &modules[1];
        assert_eq!(libc.base, 0x7f2a14c00000);
        assert_eq!(libc.size, 0x7f2a14e19000 - 0x7f2a14c00000);
    }

    #[test]
    fn data_files_and_devices_are_not_modules() {
        let modules = modules_from_maps(&parse_maps(SAMPLE));
        assert!(modules.iter().all(|m| m.name != "locale-archive"));
        assert!(modules.iter().all(|m| !m.name.contains("shared")));
    }
}
