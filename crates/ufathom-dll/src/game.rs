//! The game thread, for `[preload]`: the runtime's `ProcessEvent` hook,
//! installed only when a preload runs and removed after it.

use std::time::Duration;

use ufathom_agent::game::{GameTask, GameThreadRunner, engine_desc};
use ufathom_model::Dump;
use ufathom_runtime::inproc::GameThread;

/// Runs preload tasks on the game thread of this process.
pub(crate) struct InGame;

impl GameThreadRunner for InGame {
    fn attach(&self, dump: &Dump) -> Result<(), String> {
        if let Some(game) = GameThread::get() {
            // Installed by an earlier preload: hook the tables again.
            return game.rescan().map(|_| ()).map_err(|error| error.to_string());
        }
        let desc = Box::leak(Box::new(engine_desc(dump)));
        // SAFETY: this DLL runs inside the process `dump` was just read
        // from, so `desc` describes this exact build. The module stays
        // loaded while the hook may run: the worker unloads only after
        // `detach` reported that no thread is inside it.
        unsafe { GameThread::install(desc) }
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    fn run(&self, task: GameTask, timeout: Duration) -> Result<(), String> {
        let game = GameThread::get().ok_or("the game thread hook is not installed")?;
        game.call(timeout, move |context| task(context.executor()))
            .map_err(|error| error.to_string())
    }

    fn detach(&self, timeout: Duration) -> bool {
        let Some(game) = GameThread::get() else {
            return true;
        };
        match game.uninstall_and_wait(timeout) {
            Ok(released) => released,
            Err(error) => {
                tracing::error!("cannot remove the game thread hook: {error}");
                false
            }
        }
    }
}
