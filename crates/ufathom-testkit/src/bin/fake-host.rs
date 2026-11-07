//! `fake-host`: hosts a synthetic Unreal Engine image inside its own address
//! space, so readers can be tested through real operating system calls and a
//! DLL can later be loaded next to it.
//!
//! Usage: `fake-host --profile <name> [--seed N] [--filler N] [--corrupt <Name>]...
//! [--load <dll>]... [--report-stall]`
//!
//! The globals and the image-range data (vtables, fake code, strings) are
//! copied into large statics in this executable's own `.data`, so a scan of the
//! executable's writable section finds them and vtables resolve inside a real
//! image. Heap parts live in `VirtualAlloc`ed pages with unmapped gaps. One
//! JSON line with the addresses and the expected layout is printed to stdout,
//! then the process stays alive, ticking a counter on a thread to simulate a
//! running game, until stdin is closed.
//!
//! # Hosting a DLL
//!
//! `--load <dll>` (repeatable) calls `LoadLibraryW` on the path once the image
//! is hosted and the first line is printed, and prints one line
//! `{"loaded": {"path", "ok", "error"}}` for each. While the process runs,
//! stdin lines are commands: `load <path>` loads one more DLL and answers
//! with the same kind of line, and `report` prints a report line and starts a
//! new stall measurement window. Closing stdin ends the process.
//!
//! # Measuring the host
//!
//! The ticker thread notes the gap between its consecutive ticks. A host that
//! a DLL slows down shows a larger worst gap. `--report-stall` prints one
//! last report line when stdin is closed; the `report` command prints one at
//! any time. A report is
//! `{"report": {"worst_gap_ms", "ticks", "threads", "threads_before_load",
//! "new_thread_ids", "dlls": [{"path", "loaded"}]}}`: the worst gap since the
//! previous report (or the start), the thread count of the whole process now
//! and before the first load, the ids of threads that did not exist before
//! the first load, and whether each DLL is still mapped (`GetModuleHandleW` by
//! file name).
//!
//! Only Windows is supported. On other systems the program says so and exits
//! with status 2, because hosting needs address space reservation with gaps
//! that was not ported.

#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    reason = "a command line tool whose job is to print one JSON line"
)]

#[cfg(not(windows))]
fn main() {
    eprintln!("fake-host is only supported on Windows");
    std::process::exit(2);
}

#[cfg(windows)]
fn main() {
    host::run();
}

#[cfg(windows)]
mod host {
    use std::cell::UnsafeCell;
    use std::ffi::OsStr;
    use std::io::BufRead;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant};

    use ufathom_testkit::{
        Corruption, DATA_CAP, FixtureBuilder, HostedPlacement, PartKind, Placement, Profile,
        RDATA_CAP, TEXT_CAP,
    };
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next,
    };
    use windows_sys::Win32::System::LibraryLoader::{GetModuleHandleW, LoadLibraryW};
    use windows_sys::Win32::System::Memory::{
        MEM_COMMIT, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE, VirtualAlloc,
    };

    /// A page-aligned block of mutable static memory. It is initialised with a
    /// non-zero pattern so the linker keeps it in the file-backed `.data`.
    #[repr(C, align(4096))]
    struct Section<const N: usize>(UnsafeCell<[u8; N]>);

    // SAFETY: the blocks are written once, before any other thread exists, and
    // afterwards only through the single counter slot by one ticker thread.
    unsafe impl<const N: usize> Sync for Section<N> {}

    static TEXT: Section<TEXT_CAP> = Section(UnsafeCell::new([0xCC; TEXT_CAP]));
    static RDATA: Section<RDATA_CAP> = Section(UnsafeCell::new([0x5A; RDATA_CAP]));
    static DATA: Section<DATA_CAP> = Section(UnsafeCell::new([0xA5; DATA_CAP]));

    struct Args {
        profile: String,
        seed: u64,
        filler: Option<usize>,
        corruptions: Vec<Corruption>,
        loads: Vec<String>,
        report_stall: bool,
    }

    fn fail(message: &str) -> ! {
        eprintln!("fake-host: {message}");
        std::process::exit(1);
    }

    fn parse_args() -> Args {
        let mut args = Args {
            profile: String::new(),
            seed: 0,
            filler: None,
            corruptions: Vec::new(),
            loads: Vec::new(),
            report_stall: false,
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            if flag == "--report-stall" {
                args.report_stall = true;
                continue;
            }
            let value = it
                .next()
                .unwrap_or_else(|| fail(&format!("{flag} needs a value")));
            match flag.as_str() {
                "--profile" => args.profile = value,
                "--seed" => args.seed = value.parse().unwrap_or_else(|_| fail("bad --seed")),
                "--filler" => {
                    args.filler = Some(value.parse().unwrap_or_else(|_| fail("bad --filler")));
                }
                "--load" => args.loads.push(value),
                "--corrupt" => {
                    let found = Corruption::ALL.iter().find(|c| format!("{c:?}") == value);
                    args.corruptions.push(
                        *found.unwrap_or_else(|| fail(&format!("unknown corruption {value}"))),
                    );
                }
                other => fail(&format!("unknown argument {other}")),
            }
        }
        if args.profile.is_empty() {
            fail(
                "usage: fake-host --profile <name> [--seed N] [--filler N] [--corrupt <Name>]... [--load <dll>]... [--report-stall]",
            );
        }
        args
    }

    fn module_base() -> u64 {
        // SAFETY: a null name asks for the handle of the calling process's
        // executable; the call has no other preconditions.
        let handle = unsafe { GetModuleHandleW(std::ptr::null()) };
        handle as usize as u64
    }

    fn module_name() -> String {
        std::env::current_exe()
            .ok()
            .and_then(|path| path.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "fake-host.exe".to_owned())
    }

    fn builder(args: &Args, placement: Placement) -> FixtureBuilder {
        let profile = Profile::by_name(&args.profile)
            .unwrap_or_else(|| fail(&format!("unknown profile {}", args.profile)));
        let mut builder = FixtureBuilder::new(profile)
            .seed(args.seed)
            .placement(placement);
        if let Some(filler) = args.filler {
            builder = builder.filler_objects(filler);
        }
        for &corruption in &args.corruptions {
            builder = builder.corrupt(corruption);
        }
        builder
    }

    /// Reserves `span` bytes of address space with no access.
    fn reserve(span: u64) -> u64 {
        // SAFETY: a null base lets the system pick the address; reserving
        // address space has no preconditions and touches no existing memory.
        let base =
            unsafe { VirtualAlloc(std::ptr::null(), span as usize, MEM_RESERVE, PAGE_NOACCESS) };
        if base.is_null() {
            fail("could not reserve the heap range");
        }
        base as usize as u64
    }

    /// Commits `len` bytes at `addr`, inside the reserved range.
    fn commit(addr: u64, len: usize) {
        // SAFETY: `addr..addr + len` lies inside the range reserved by
        // `reserve` and does not overlap another committed part.
        let got =
            unsafe { VirtualAlloc(addr as usize as *const _, len, MEM_COMMIT, PAGE_READWRITE) };
        if got.is_null() {
            fail("could not commit a heap part");
        }
    }

    /// Copies `bytes` to `addr`.
    ///
    /// # Safety
    ///
    /// `addr..addr + bytes.len()` must be writable memory owned by this
    /// process that nothing else is reading or writing.
    unsafe fn copy_to(addr: u64, bytes: &[u8]) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), addr as usize as *mut u8, bytes.len());
        }
    }

    pub fn run() {
        let args = parse_args();
        let name = module_name();
        let base = module_base();
        let text = TEXT.0.get() as usize as u64;
        let rdata = RDATA.0.get() as usize as u64;
        let data = DATA.0.get() as usize as u64;

        // Sizes never depend on addresses, so build once with a dummy heap to
        // learn how much address space to reserve.
        let dummy = HostedPlacement {
            module_name: name.clone(),
            module_base: base,
            text,
            rdata,
            data,
            heap: 0x1_0000_0000,
        };
        let probe = builder(&args, Placement::Hosted(dummy.clone())).build();
        let heap = reserve(probe.heap_span);
        let placement = HostedPlacement { heap, ..dummy };
        let fixture = builder(&args, Placement::Hosted(placement)).build();
        if fixture.heap_span != probe.heap_span || fixture.parts.len() != probe.parts.len() {
            fail("image size changed with the placement");
        }

        for part in &fixture.parts {
            let capacity = match part.kind {
                PartKind::Text => TEXT_CAP,
                PartKind::Rdata => RDATA_CAP,
                PartKind::Data => DATA_CAP,
                _ => {
                    commit(part.addr, part.bytes.len());
                    usize::MAX
                }
            };
            if part.bytes.len() > capacity {
                fail("an image section does not fit its static");
            }
            // SAFETY: the target is either a static of at least `capacity`
            // bytes or a heap part committed just above, and no other thread
            // exists yet.
            unsafe { copy_to(part.addr, &part.bytes) };
        }

        let counter = fixture.anchors.data.frame_counter;
        let line = serde_json::json!({
            "profile": fixture.profile.name,
            "seed": args.seed,
            "pid": std::process::id(),
            "module": name,
            "module_base": base,
            "layout": fixture.layout,
            "globals": {
                "module": fixture.globals.module,
                "gworld_rva": fixture.globals.gworld_rva,
                "gengine_rva": fixture.globals.gengine_rva,
                "process_event_index": fixture.globals.process_event_index,
                "process_event_rva": fixture.globals.process_event_rva,
            },
            "engine": {
                "claimed": fixture.engine.claimed,
                "build": fixture.engine.build,
                "changelist": fixture.engine.changelist,
            },
            "addresses": {
                "text": text,
                "rdata": rdata,
                "data": data,
                "heap": heap,
                "heap_span": fixture.heap_span,
                "object_array": fixture.anchors.data.object_array,
                "name_anchor": fixture.anchors.data.name_anchor,
                "gworld": fixture.anchors.data.gworld,
                "gengine": fixture.anchors.data.gengine,
                "frame_counter": counter,
                "world": fixture.anchors.world,
                "engine": fixture.anchors.engine,
                "unmapped": fixture.anchors.unmapped,
            },
        });
        println!("{line}");

        let stop = Arc::new(AtomicBool::new(false));
        let ticks = Arc::new(AtomicU64::new(0));
        let worst_gap_micros = Arc::new(AtomicU64::new(0));
        let ticker = {
            let stop = Arc::clone(&stop);
            let ticks = Arc::clone(&ticks);
            let worst_gap_micros = Arc::clone(&worst_gap_micros);
            std::thread::spawn(move || {
                let mut last = Instant::now();
                while !stop.load(Ordering::Relaxed) {
                    let now = Instant::now();
                    let gap =
                        u64::try_from(now.duration_since(last).as_micros()).unwrap_or(u64::MAX);
                    last = now;
                    worst_gap_micros.fetch_max(gap, Ordering::Relaxed);
                    let tick = ticks.fetch_add(1, Ordering::Relaxed) + 1;
                    // SAFETY: `counter` is an 8-byte aligned slot inside the
                    // `DATA` static; this thread is the only writer.
                    unsafe { std::ptr::write_volatile(counter as usize as *mut u64, tick) };
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };

        let mut watch = Watch {
            ticks,
            worst_gap_micros,
            baseline: thread_ids(),
            dlls: Vec::new(),
        };
        for path in &args.loads {
            load_dll(&mut watch, path);
        }

        // Commands arrive one per line; closing stdin ends the process.
        let stdin = std::io::stdin();
        let mut line = String::new();
        loop {
            line.clear();
            match stdin.lock().read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            let command = line.trim();
            if let Some(path) = command.strip_prefix("load ") {
                load_dll(&mut watch, path.trim());
            } else if command == "report" {
                println!("{}", watch.report());
            }
        }
        if args.report_stall {
            println!("{}", watch.report());
        }
        stop.store(true, Ordering::Relaxed);
        let _ = ticker.join();
    }

    /// What the host measures about itself and the DLLs it was asked to load.
    struct Watch {
        ticks: Arc<AtomicU64>,
        worst_gap_micros: Arc<AtomicU64>,
        /// Thread ids before the first load.
        baseline: Vec<u32>,
        /// Paths handed to `LoadLibraryW`, loaded or not.
        dlls: Vec<String>,
    }

    impl Watch {
        /// A report line. Starts a new stall window.
        fn report(&self) -> serde_json::Value {
            let worst = self.worst_gap_micros.swap(0, Ordering::Relaxed);
            let now = thread_ids();
            let new_ids: Vec<u32> = now
                .iter()
                .copied()
                .filter(|id| !self.baseline.contains(id))
                .collect();
            let dlls: Vec<_> = self
                .dlls
                .iter()
                .map(|path| {
                    serde_json::json!({
                        "path": path,
                        "loaded": is_loaded(path),
                    })
                })
                .collect();
            serde_json::json!({
                "report": {
                    "worst_gap_ms": worst as f64 / 1000.0,
                    "ticks": self.ticks.load(Ordering::Relaxed),
                    "threads": now.len(),
                    "threads_before_load": self.baseline.len(),
                    "new_thread_ids": new_ids,
                    "dlls": dlls,
                }
            })
        }
    }

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    /// Loads a DLL on the calling thread, as a game's own loader would, and
    /// prints one line with the result.
    fn load_dll(watch: &mut Watch, path: &str) {
        watch.dlls.push(path.to_owned());
        let wide_path = wide(OsStr::new(path));
        // SAFETY: `wide_path` is a null-terminated UTF-16 string that outlives
        // the call. Loading a library runs its `DllMain`; the DLLs given to a
        // test host are the ones under test.
        let module = unsafe { LoadLibraryW(wide_path.as_ptr()) };
        let error = if module.is_null() {
            // SAFETY: reads the calling thread's last error; no preconditions.
            unsafe { GetLastError() }
        } else {
            0
        };
        let line = serde_json::json!({
            "loaded": { "path": path, "ok": !module.is_null(), "error": error }
        });
        println!("{line}");
    }

    /// Whether a module with the file name of `path` is mapped in this process.
    fn is_loaded(path: &str) -> bool {
        let Some(name) = Path::new(path).file_name() else {
            return false;
        };
        let wide_name = wide(name);
        // SAFETY: `wide_name` is a null-terminated UTF-16 string that outlives
        // the call; `GetModuleHandleW` does not change the module's reference
        // count.
        !unsafe { GetModuleHandleW(wide_name.as_ptr()) }.is_null()
    }

    /// Ids of every thread of this process, from a toolhelp snapshot. Empty if
    /// the snapshot cannot be taken.
    fn thread_ids() -> Vec<u32> {
        let pid = std::process::id();
        // SAFETY: a thread snapshot with process id 0 covers all processes; the
        // call has no preconditions and returns a handle we close below.
        let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
        if snapshot == INVALID_HANDLE_VALUE {
            return Vec::new();
        }
        let mut ids = Vec::new();
        let mut entry = THREADENTRY32 {
            dwSize: size_of::<THREADENTRY32>() as u32,
            cntUsage: 0,
            th32ThreadID: 0,
            th32OwnerProcessID: 0,
            tpBasePri: 0,
            tpDeltaPri: 0,
            dwFlags: 0,
        };
        // SAFETY: `snapshot` is a valid snapshot handle and `entry` is a
        // `THREADENTRY32` whose `dwSize` is set, as the API requires.
        let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
        while more {
            if entry.th32OwnerProcessID == pid {
                ids.push(entry.th32ThreadID);
            }
            entry.dwSize = size_of::<THREADENTRY32>() as u32;
            // SAFETY: as for `Thread32First`.
            more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
        }
        // SAFETY: `snapshot` is a handle we own and have not closed.
        unsafe { CloseHandle(snapshot) };
        ids.sort_unstable();
        ids
    }
}
