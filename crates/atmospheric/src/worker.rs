//! Background worker thread: the only place network and disk I/O happens.
//!
//! The game side never awaits anything. It pushes [`Command`]s through one
//! channel and drains [`Event`]s from another once per tick; this thread owns
//! the tokio runtime, the AT Protocol agent, and the settings and cursor
//! files. Keeping that boundary is what makes the rest of the crate testable
//! without a game.

use crate::{Command, Event};
use std::sync::mpsc::{Receiver, Sender};

/// Spawn the worker thread.
///
/// `commands` is what the game sends us, `events` is what we report back.
/// Dropping the command channel (or sending [`Command::Shutdown`]) stops the
/// thread.
pub fn spawn(commands: Receiver<Command>, events: Sender<Event>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            let _ = events.send(Event::Failed {
                message: "could not start the worker runtime".into(),
            });
            return;
        };
        runtime.block_on(async move {
            while let Ok(command) = commands.recv() {
                match command {
                    Command::Shutdown => break,
                    Command::Login
                    | Command::PostText { .. }
                    | Command::PostImage { .. }
                    | Command::RefreshNotifications => {
                        // TODO(M2): wire to session::login and actions::* now
                        // that the runtime is running. Until then the
                        // pipeline is intentionally inert.
                        let _ = events.send(Event::Failed {
                            message: "network actions arrive in M2".into(),
                        });
                    }
                }
            }
        });
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn worker_reports_and_shuts_down() {
        let (command_tx, command_rx) = mpsc::channel::<Command>();
        let (event_tx, event_rx) = mpsc::channel::<Event>();

        let handle = spawn(command_rx, event_tx);
        command_tx.send(Command::PostText { text: "hi".into() }).unwrap();
        command_tx.send(Command::Shutdown).unwrap();

        let mut saw_inert_reply = false;
        while let Ok(event) = event_rx.recv_timeout(std::time::Duration::from_secs(5)) {
            if let Event::Failed { .. } = event {
                saw_inert_reply = true;
            }
        }
        assert!(handle.join().is_ok());
        assert!(saw_inert_reply);
    }
}
