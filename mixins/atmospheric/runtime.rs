// Runtime module for the atmospheric native mod.
//
// This exact source is also injected as a same-crate module by horizon-mixins
// (as `crate::game::atmospheric`), so it must stay std-only: no external
// crates, no game imports. All AT Protocol work happens off-thread in the
// `atmospheric` crate (crates/atmospheric), reached through the channels
// installed here.

use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Mutex, OnceLock};

/// Commands the game side sends to the worker thread.
#[derive(Debug, Clone)]
pub enum GameCommand {
    /// The player broke a block (placeholder hook until keybinds land).
    BlockBroken { pos: [i32; 3] },
    /// Stop the worker thread.
    Shutdown,
}

/// Events the worker thread reports back to the game side.
#[derive(Debug, Clone)]
pub enum ModEvent {
    Status(String),
}

static COMMAND_TX: OnceLock<Sender<GameCommand>> = OnceLock::new();
static EVENTS_RX: OnceLock<Mutex<Receiver<ModEvent>>> = OnceLock::new();

/// Create the channel pair and install the game side.
///
/// Returns the worker side — the receiving end of the commands and the
/// sending end of the events — exactly once. A second call returns `None`.
pub fn install() -> Option<(Receiver<GameCommand>, Sender<ModEvent>)> {
    if COMMAND_TX.get().is_some() {
        return None;
    }
    let (command_tx, command_rx) = std::sync::mpsc::channel();
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let _ = COMMAND_TX.set(command_tx);
    if EVENTS_RX.set(Mutex::new(event_rx)).is_err() {
        return None; // Already installed.
    }
    Some((command_rx, event_tx))
}

/// Drain pending worker events, oldest first.
///
/// The game calls this once per tick; each drain is bounded so the game
/// thread never stalls on the worker.
pub fn drain_events(max: usize) -> Vec<ModEvent> {
    let mut drained = Vec::new();
    if let Some(rx) = EVENTS_RX.get() {
        if let Ok(rx) = rx.lock() {
            while drained.len() < max {
                match rx.try_recv() {
                    Ok(event) => drained.push(event),
                    Err(_) => break,
                }
            }
        }
    }
    drained
}

/// Forward a block break to the worker, if installed. Placeholder hook: it
/// proves the injection pipeline end to end before real keybinds arrive.
pub fn before_break(pos: [i32; 3]) {
    if let Some(tx) = COMMAND_TX.get() {
        let _ = tx.send(GameCommand::BlockBroken { pos });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The channel endpoints live in statics, so the tests share one entry
    // point instead of racing over them in parallel.
    #[test]
    fn install_forwards_and_drains() {
        let (command_rx, event_tx) = install().expect("first install succeeds");
        assert!(install().is_none(), "second install fails");

        // Game side -> worker: a forwarded block break.
        before_break([-3, 64, 12]);
        match command_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(GameCommand::BlockBroken { pos }) => assert_eq!(pos, [-3, 64, 12]),
            other => panic!("expected BlockBroken, got {:?}", other),
        }

        // Worker -> game side: a drained event.
        event_tx.send(ModEvent::Status("logged out".into())).unwrap();
        event_tx.send(ModEvent::Status("logged in".into())).unwrap();
        let drained = drain_events(16);
        assert_eq!(drained.len(), 2);

        // The drain is bounded.
        event_tx.send(ModEvent::Status("overflow".into())).unwrap();
        assert_eq!(drain_events(1).len(), 1);
        assert!(drain_events(16).is_empty());

        // The worker side stays usable after the game side drops it.
        drop(event_tx);
        before_break([0, 0, 0]);
    }
}
