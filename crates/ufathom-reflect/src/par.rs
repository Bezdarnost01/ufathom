//! Running work on the caller's pool, or on the calling thread.

use rayon::ThreadPool;
use rayon::prelude::*;

/// Where parallel phases run. Never the global rayon pool.
#[derive(Clone, Copy)]
pub(crate) struct Exec<'a>(pub(crate) Option<&'a ThreadPool>);

impl Exec<'_> {
    /// Applies `f` to consecutive slices of `items` of at most `chunk`
    /// elements and returns the results in slice order.
    ///
    /// The order of the result does not depend on the pool or on scheduling.
    pub(crate) fn map_chunks<T, R, F>(&self, items: &[T], chunk: usize, f: F) -> Vec<R>
    where
        T: Sync,
        R: Send,
        F: Fn(&[T]) -> R + Sync + Send,
    {
        let chunk = chunk.max(1);
        match self.0 {
            Some(pool) => pool.install(|| items.par_chunks(chunk).map(&f).collect()),
            None => items.chunks(chunk).map(f).collect(),
        }
    }
}
