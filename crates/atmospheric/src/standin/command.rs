//! Pinned `horizon-ctl` command catalog and the local-deny classifier (#206).
//!
//! Agent control is preview and unversioned upstream, so a doorkeeper cannot
//! safely forward a name it has never seen. The catalog below is the command
//! column of `docs/horizon-ctl-commands.md` at the Horizon revision pinned in
//! `compatibility.json`: a name outside it is `unknown` and denied. Names in
//! the lifecycle, multiplayer, scripting, benchmark, recording, file-writing
//! and cheat classes are denied even when a grant lists them.
//!
//! A delegated command is an exact command line, not a command name with
//! unrestricted arguments. The doorkeeper forwards only the exact string in
//! the grant; this module decides whether that string is eligible at all.
//! `horizon-ctl` splits lines like a shell (quotes keep spaces, backslash
//! escapes, `#` starts a comment), so any line whose meaning could change
//! under that split is rejected rather than interpreted.

/// Horizon revision the catalog was generated from, kept beside the catalog so
/// a future pin update forces a deliberate review.
pub const CATALOG_REVISION: &str = "9f42064c11efe7ef3e5ed2baad7ddc2a3b36eea0";

/// Maximum length of a delegated command line, in bytes.
pub const MAX_COMMAND_LEN: usize = 512;

/// Every command name in the pinned generated reference, sorted bytewise.
pub const KNOWN_COMMANDS: &[&str] = &[
    "attack",
    "bench",
    "block",
    "blocks",
    "book",
    "break",
    "chat",
    "chat-log",
    "click",
    "click-at",
    "click-button",
    "click-slot",
    "close",
    "collect",
    "command",
    "connect",
    "container",
    "disconnect",
    "drag-at",
    "drop",
    "effect",
    "effects",
    "emote",
    "enchant",
    "entities",
    "events",
    "fill",
    "find-block",
    "fly",
    "friends",
    "give",
    "goto",
    "gpu-passes",
    "hello",
    "hold",
    "hunt",
    "input-log",
    "interact",
    "interact-entity",
    "inventory",
    "jump",
    "key",
    "lan",
    "launch",
    "light",
    "logs",
    "look",
    "look-at",
    "look-at-block",
    "look-at-entity",
    "looking-at",
    "mcp",
    "memory",
    "menu",
    "messages",
    "mount-menu",
    "mouse",
    "mouse-move",
    "move",
    "narration",
    "open",
    "open-inventory",
    "options",
    "options-get",
    "options-set",
    "pad",
    "pause",
    "place",
    "potion",
    "profile",
    "quit",
    "record-list",
    "record-replay",
    "record-start",
    "record-status",
    "record-stop",
    "release",
    "render-capture",
    "render-distance",
    "repro-export",
    "respawn",
    "resume",
    "save",
    "screen",
    "screenshot",
    "screenshot-sequence",
    "script",
    "scroll-at",
    "select",
    "servers",
    "set",
    "setblock",
    "signedit",
    "sleep",
    "state",
    "stats",
    "status",
    "stop",
    "summon",
    "time",
    "tp",
    "trace",
    "turn",
    "type",
    "ui",
    "use-item",
    "wait",
    "wait-for",
    "wait-ready",
    "world-backup",
    "world-delete",
    "world-rename",
    "worlds",
    "xp",
];

/// Names a delegation never receives. `break` is deliberately *not* here:
/// harvesting needs it, and an exact grant line already limits it to one
/// block.
pub const DENIED_COMMANDS: &[&str] = &[
    // Session and world lifecycle.
    "launch",
    "open",
    "quit",
    "save",
    "world-backup",
    "world-delete",
    "world-rename",
    // Multiplayer, LAN and friends: joining, leaving or exposing a world.
    "connect",
    "disconnect",
    "lan",
    "friends",
    // Scripting, blocking waits and a nested control server.
    "mcp",
    "script",
    "sleep",
    "wait",
    "wait-for",
    "wait-ready",
    // Raw input that outlives one exact line.
    "hold",
    "key",
    "pad",
    "mouse",
    // Recording and repro bundles.
    "record-list",
    "record-replay",
    "record-start",
    "record-status",
    "record-stop",
    "repro-export",
    // Benchmarks, tracing and anything that writes files or changes settings.
    "bench",
    "fly",
    "gpu-passes",
    "logs",
    "options-set",
    "profile",
    "render-capture",
    "render-distance",
    "screenshot",
    "screenshot-sequence",
    "trace",
    // Arbitrary server commands and cheats.
    "command",
    "effect",
    "enchant",
    "fill",
    "give",
    "potion",
    "setblock",
    "summon",
    "time",
    "tp",
    "xp",
];

/// Eligibility of a command line at the doorkeeper boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandClass {
    /// The name is pinned and not in a denied class. A matching grant line is
    /// still required before anything may run.
    Delegable,
    /// The name does not appear in the pinned catalog.
    Unknown,
    /// The name, or this use of it, is categorically denied to agents.
    Denied,
    /// `chat` or `type` would run a server command instead of plain text.
    CommandInText,
    /// The line asks for the upstream destructive confirmation bypass.
    Confirm,
    /// The line is empty, too long, not printable ASCII, or carries shell
    /// quoting, escapes or comments.
    Malformed,
}

impl CommandClass {
    /// Stable wire name, used in deny reasons and audit receipts.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delegable => "delegable",
            Self::Unknown => "unknown_command",
            Self::Denied => "denied_command",
            Self::CommandInText => "command_in_text",
            Self::Confirm => "confirm_required",
            Self::Malformed => "malformed_command",
        }
    }
}

/// Whether a name belongs to the pinned command catalog.
pub fn is_known(name: &str) -> bool {
    KNOWN_COMMANDS.binary_search(&name).is_ok()
}

/// Classify an exact command line without executing it.
pub fn classify(line: &str) -> CommandClass {
    if line.is_empty()
        || line.len() > MAX_COMMAND_LEN
        || !line.bytes().all(|byte| (b' '..=b'~').contains(&byte))
        || line.contains(['"', '\'', '\\', '#'])
    {
        return CommandClass::Malformed;
    }
    let mut words = line.split_whitespace();
    let Some(name) = words.next() else {
        return CommandClass::Malformed;
    };
    if !is_known(name) {
        return CommandClass::Unknown;
    }
    if DENIED_COMMANDS.contains(&name) {
        return CommandClass::Denied;
    }
    let args: Vec<&str> = words.collect();
    if args.iter().any(|word| word.starts_with("--confirm")) {
        return CommandClass::Confirm;
    }
    if (name == "chat" || name == "type") && args.iter().any(|word| word.starts_with('/')) {
        return CommandClass::CommandInText;
    }
    // `container X Y Z` lists a container; `container X Y Z ITEM COUNT` adds
    // items to it, which is a cheat.
    if name == "container" && args.len() > 3 {
        return CommandClass::Denied;
    }
    CommandClass::Delegable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_sorted_and_pinned_to_compatibility() {
        let compatibility = include_str!("../../../../compatibility.json");
        assert!(
            compatibility.contains(&format!("\"horizon_revision\": \"{CATALOG_REVISION}\"")),
            "the command catalog must be regenerated when the Horizon pin moves"
        );
        assert_eq!(KNOWN_COMMANDS.len(), 114);
        assert!(KNOWN_COMMANDS.windows(2).all(|pair| pair[0] < pair[1]));
        for name in ["state", "move", "break", "interact", "chat", "use-item"] {
            assert!(is_known(name), "{name} should be pinned");
        }
        // Reply fields are not commands.
        for name in ["yaw", "fps", "day_time", "render_distance"] {
            assert!(!is_known(name), "{name} is a reply field");
        }
    }

    #[test]
    fn every_denied_name_is_a_pinned_command() {
        for name in DENIED_COMMANDS {
            assert!(is_known(name), "{name} is denied but not pinned");
            assert_eq!(classify(name), CommandClass::Denied, "{name}");
        }
    }

    #[test]
    fn unknown_commands_fail_closed() {
        assert_eq!(classify("not-a-command"), CommandClass::Unknown);
        assert_eq!(classify("STATE"), CommandClass::Unknown);
        assert_eq!(classify("yaw"), CommandClass::Unknown);
        assert_eq!(classify(" "), CommandClass::Malformed);
        assert_eq!(classify(""), CommandClass::Malformed);
    }

    #[test]
    fn dangerous_commands_are_denied() {
        assert_eq!(classify("setblock 1 2 3 stone"), CommandClass::Denied);
        assert_eq!(
            classify("world-delete world --confirm"),
            CommandClass::Denied
        );
        assert_eq!(classify("give diamond 64"), CommandClass::Denied);
        assert_eq!(classify("command kill"), CommandClass::Denied);
        assert_eq!(classify("screenshot /tmp/x.png"), CommandClass::Denied);
        assert_eq!(classify("container 1 2 3"), CommandClass::Delegable);
        assert_eq!(classify("container 1 2 3 diamond 64"), CommandClass::Denied);
    }

    #[test]
    fn text_commands_cannot_hide_server_commands() {
        assert_eq!(classify("chat hello"), CommandClass::Delegable);
        assert_eq!(classify("chat /kill"), CommandClass::CommandInText);
        assert_eq!(classify("chat hi /op me"), CommandClass::CommandInText);
        assert_eq!(
            classify("type --replace /give me diamond"),
            CommandClass::CommandInText
        );
    }

    #[test]
    fn shell_syntax_and_confirm_bypasses_fail_closed() {
        assert_eq!(classify("chat \"/op me\""), CommandClass::Malformed);
        assert_eq!(classify("chat '/op me'"), CommandClass::Malformed);
        assert_eq!(classify("chat \\/op"), CommandClass::Malformed);
        assert_eq!(classify("state # comment"), CommandClass::Malformed);
        assert_eq!(classify("move forward 2\nstop"), CommandClass::Malformed);
        assert_eq!(classify("move\tforward 2"), CommandClass::Malformed);
        assert_eq!(classify("chat héllo"), CommandClass::Malformed);
        assert_eq!(
            classify(&format!("chat {}", "a".repeat(MAX_COMMAND_LEN))),
            CommandClass::Malformed
        );
        assert_eq!(classify("break 1 2 3 --confirm"), CommandClass::Confirm);
        assert_eq!(
            classify("break 1 2 3 --confirm=true"),
            CommandClass::Confirm
        );
    }

    #[test]
    fn wire_names_are_stable() {
        assert_eq!(CommandClass::Delegable.as_str(), "delegable");
        assert_eq!(CommandClass::Unknown.as_str(), "unknown_command");
        assert_eq!(CommandClass::Denied.as_str(), "denied_command");
        assert_eq!(CommandClass::CommandInText.as_str(), "command_in_text");
        assert_eq!(CommandClass::Confirm.as_str(), "confirm_required");
        assert_eq!(CommandClass::Malformed.as_str(), "malformed_command");
    }
}
