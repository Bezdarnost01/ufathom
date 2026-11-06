//! `fake-host`: hosts a synthetic Unreal Engine image inside its own address
//! space, so readers can be tested through real operating system calls and a
//! DLL can later be loaded next to it.
//!
//! Usage: `fake-host --profile <name> [--seed N] [--filler N] [--corrupt <Name>]...`
//!
//! The globals and the image-range data (vtables, fake code, strings) are
//! copied into large statics in this executable's own `.data`, so a scan of the
//! executable's writable section finds them and vtables resolve inside a real
//! image. Heap parts live in `VirtualAlloc`ed pages with unmapped gaps. One
//! JSON line with the addresses and the expected layout is printed to stdout,
//! then the process stays alive, ticking a counter on a thread to simulate a
//! running game, until stdin is closed.
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
    use std::io::Read;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use ufathom_testkit::{
        Corruption, DATA_CAP, FixtureBuilder, HostedPlacement, PartKind, Placement, Profile,
        RDATA_CAP, TEXT_CAP,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
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
        };
        let mut it = std::env::args().skip(1);
        while let Some(flag) = it.next() {
            let value = it
                .next()
                .unwrap_or_else(|| fail(&format!("{flag} needs a value")));
            match flag.as_str() {
                "--profile" => args.profile = value,
                "--seed" => args.seed = value.parse().unwrap_or_else(|_| fail("bad --seed")),
                "--filler" => {
                    args.filler = Some(value.parse().unwrap_or_else(|_| fail("bad --filler")));
                }
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
            fail("usage: fake-host --profile <name> [--seed N] [--filler N] [--corrupt <Name>]...");
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
        let ticker = {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut ticks: u64 = 0;
                while !stop.load(Ordering::Relaxed) {
                    ticks += 1;
                    // SAFETY: `counter` is an 8-byte aligned slot inside the
                    // `DATA` static; this thread is the only writer.
                    unsafe { std::ptr::write_volatile(counter as usize as *mut u64, ticks) };
                    std::thread::sleep(Duration::from_millis(5));
                }
            })
        };
        let mut sink = Vec::new();
        let _ = std::io::stdin().read_to_end(&mut sink);
        stop.store(true, Ordering::Relaxed);
        let _ = ticker.join();
    }
}
