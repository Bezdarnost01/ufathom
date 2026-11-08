//! `ufathom dump`: discovery, reflection and the files that come from them.

use std::time::Instant;

use anyhow::{Context, anyhow};
use rayon::ThreadPoolBuilder;
use ufathom_codegen::GenOptions;
use ufathom_reflect::{ReflectOptions, build_dump};
use ufathom_usmap::write_usmap;

use crate::args::DumpArgs;
use crate::commands::io::{generate_sdk, print_gen_report, runtime_options, size, write_file};
use crate::error::{CliError, Outcome, usage};
use crate::target::Target;

/// `ufathom dump`.
pub fn dump(args: &DumpArgs) -> Outcome {
    // Everything that can be wrong with the arguments is found before the
    // target is touched.
    let sdk_options = sdk_options(args)?;
    let threads = match args.threads {
        Some(0) => return Err(usage("--threads must be at least 1")),
        Some(count) => count,
        None => std::thread::available_parallelism().map_or(1, usize::from),
    };

    let target = Target::open(&args.target)?;
    let pool = ThreadPoolBuilder::new()
        .num_threads(threads)
        .thread_name(|index| format!("ufathom-worker-{index}"))
        .build()
        .context("cannot start the worker threads")?;

    let started = Instant::now();
    let discovery = target.discover(Some(&pool))?;
    let discovered = started.elapsed();
    tracing::info!(
        engine = discovery.engine.claimed.as_deref().unwrap_or("unknown"),
        "layout solved in {discovered:.1?}"
    );
    for warning in &discovery.warnings {
        tracing::info!("discovery warning: {warning}");
    }

    let options = ReflectOptions {
        pool: Some(&pool),
        ..ReflectOptions::default()
    };
    let output = build_dump(&*target.reader, &discovery, &options)
        .map_err(|error| anyhow!("reflection failed on {}: {error}", target.description))?;
    let reflected = started.elapsed().saturating_sub(discovered);
    let stats = &output.stats;

    println!("dump of {}", target.description);
    println!(
        "engine:        {}",
        discovery
            .engine
            .claimed
            .as_deref()
            .unwrap_or("not claimed by the binary")
    );
    println!("objects:       {}", stats.objects);
    println!(
        "types:         {} classes, {} script structs, {} enums, {} functions",
        stats.classes, stats.structs, stats.enums, stats.functions
    );
    println!(
        "problems:      {} objects failed, {} diagnostics, {} discovery warnings (`ufathom probe` lists them)",
        stats.failures,
        output.dump.diagnostics.len(),
        discovery.warnings.len()
    );
    println!(
        "degraded:      {}",
        if output.degraded { "YES" } else { "no" }
    );

    let json = output
        .dump
        .to_json()
        .map_err(|error| anyhow!("cannot serialize the dump: {error}"))?;
    write_file(&args.output, json.as_bytes())?;
    println!(
        "wrote:         {} ({})",
        args.output.display(),
        size(json.len())
    );
    if let Some(path) = &args.usmap {
        let usmap = write_usmap(&output.dump);
        write_file(path, &usmap)?;
        println!("wrote:         {} ({})", path.display(), size(usmap.len()));
    }
    if let (Some(folder), Some(options)) = (&args.sdk, &sdk_options) {
        let report = generate_sdk(&output.dump, options, folder)?;
        print_gen_report(&report, folder);
    }
    println!(
        "time:          {:.1?} (discovery {discovered:.1?}, reflection {reflected:.1?}, {threads} threads)",
        started.elapsed()
    );

    if output.degraded && !args.allow_degraded {
        return Err(CliError::Degraded(format!(
            "the dump is degraded: {} of {} objects could not be dumped completely. \
             The files were written; read the diagnostics in {} before trusting them, \
             or pass --allow-degraded to accept the result",
            stats.failures,
            stats.objects,
            args.output.display()
        )));
    }
    Ok(())
}

/// The options for `--sdk`, or `None` when no SDK is asked for.
fn sdk_options(args: &DumpArgs) -> Result<Option<GenOptions>, CliError> {
    let runtime_given =
        args.runtime.runtime_path.is_some() || args.runtime.runtime_version.is_some();
    match (&args.sdk, runtime_given) {
        (Some(_), _) => runtime_options(&args.runtime).map(Some),
        (None, true) => Err(usage(
            "--runtime-path and --runtime-version only apply together with --sdk",
        )),
        (None, false) => Ok(None),
    }
}
