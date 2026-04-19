//! Helpers for managing a `pipewire-module-filter-chain` instance via
//! drop-in config files and `pw-cli`.
//!
//! Why subprocess + config drop-in rather than dynamic `load-module`:
//! `pw-cli load-module` loads the module for the lifetime of the
//! `pw-cli` process — when it exits, the module unloads. Spawning a
//! long-lived `pw-cli` child is fragile (no clean stdin protocol, log
//! interleaving, hard to recover on daemon restart). The drop-in
//! config approach (`~/.config/pipewire/filter-chain.conf.d/...`)
//! is what `pipewire` itself documents for persistent filter chains:
//! the file is loaded automatically every time `PipeWire` starts, and
//! survives daemon restarts cleanly.
//!
//! Trade-off: the user has to restart PipeWire/WirePlumber once after
//! the daemon writes a new config (the restart is one
//! `systemctl --user restart` invocation). Subsequent runtime
//! parameter changes go through `pw-cli set-param` and take effect
//! immediately without a restart.

use std::path::PathBuf;
use std::process::Command;

use thiserror::Error;
use tracing::{debug, warn};

/// Where Undertone writes its filter-chain config drop-in.
///
/// Honors `XDG_CONFIG_HOME` if set; otherwise falls back to
/// `$HOME/.config`. Filename is prefixed `50-` so it loads after any
/// distro defaults but ahead of user overrides at `90-`.
#[must_use]
pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map_or_else(
        || {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(".config")
        },
        PathBuf::from,
    );
    base.join("pipewire/filter-chain.conf.d/50-undertone-mic.conf")
}

/// Errors from filter-chain operations.
#[derive(Debug, Error)]
pub enum FilterChainError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("pw-cli failed: {0}")]
    PwCli(String),
    #[error("filter-chain node `{0}` not found in PipeWire — is the chain loaded?")]
    NodeNotFound(String),
}

/// Write the filter-chain config to the standard drop-in location,
/// creating the parent directory if needed.
///
/// Returns the path written. The caller is responsible for asking the
/// user to restart PipeWire/WirePlumber if this is the first install
/// (the daemon can detect that case by checking whether the chain's
/// virtual node appears in `GraphManager` after a short delay).
///
/// # Errors
/// Returns [`FilterChainError::Io`] if the file system rejects the
/// write or the parent directory can't be created.
pub fn install_config(content: &str) -> Result<PathBuf, FilterChainError> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Skip the write if the content already matches — avoids touching
    // mtime and tempting WirePlumber to think anything changed.
    if let Ok(existing) = std::fs::read_to_string(&path)
        && existing == content
    {
        debug!(path = %path.display(), "filter-chain config unchanged");
        return Ok(path);
    }
    std::fs::write(&path, content)?;
    debug!(path = %path.display(), "filter-chain config written");
    Ok(path)
}

/// Set a single control on a node inside the filter chain via
/// `pw-cli set-param <node-id> Props '{params=["<control>", <value>]}'`.
///
/// `node_name` is the filter-chain node identifier (e.g. `comp`,
/// `gate`, `eq`, `ns`) — these are the inner `name = ...` fields in
/// the filter-graph config, distinct from the outer `node.name` of
/// the wrapping `PipeWire` node.
///
/// # Errors
/// Returns [`FilterChainError::NodeNotFound`] if the named node isn't
/// in the registry, or [`FilterChainError::PwCli`] if `pw-cli` exits
/// non-zero.
pub fn set_control(node_id: u32, control_name: &str, value: f32) -> Result<(), FilterChainError> {
    // Build the SPA-JSON Props object expected by pw-cli set-param.
    // Format: `Props '{params = ["<key>", <value>]}'`.
    let props = format!("{{ params = [ \"{control_name}\" {value} ] }}");
    let output = Command::new("pw-cli")
        .args(["set-param", &node_id.to_string(), "Props", &props])
        .output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        warn!(
            node = node_id,
            control = control_name,
            value,
            stderr = %stderr,
            "pw-cli set-param failed"
        );
        return Err(FilterChainError::PwCli(stderr));
    }
    Ok(())
}

/// Look up a `PipeWire` node ID by name via `pw-cli ls Node`. Used as a
/// fallback when [`crate::graph::GraphManager`] doesn't yet have the
/// node cached (e.g. immediately after `PipeWire` restart).
///
/// # Errors
/// Returns [`FilterChainError::PwCli`] if the subprocess fails or
/// [`FilterChainError::NodeNotFound`] if no node with that name is
/// registered.
pub fn lookup_node_id(node_name: &str) -> Result<u32, FilterChainError> {
    let output = Command::new("pw-cli").args(["ls", "Node"]).output()?;
    if !output.status.success() {
        return Err(FilterChainError::PwCli(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_node_id(&stdout, node_name)
        .ok_or_else(|| FilterChainError::NodeNotFound(node_name.to_string()))
}

/// `pw-cli ls Node` output parser. Looks for a block whose
/// `node.name = "<wanted>"` line matches and returns the preceding
/// `id <N>,` value. Visible for testing.
#[must_use]
pub fn parse_node_id(output: &str, wanted: &str) -> Option<u32> {
    // The format looks like:
    //   id 42, type PipeWire:Interface:Node/3
    //       object.serial = "1234"
    //       node.name = "ut-mic-processed"
    //       ...
    // We scan each `id N,` block and return the id whose body
    // contains `node.name = "<wanted>"`.
    let mut current_id: Option<u32> = None;
    for line in output.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("id ")
            && let Some(id_str) = rest.split(',').next()
            && let Ok(id) = id_str.trim().parse::<u32>()
        {
            current_id = Some(id);
            continue;
        }
        if let Some(id) = current_id
            && trimmed.starts_with("node.name")
            && trimmed.contains(&format!("\"{wanted}\""))
        {
            return Some(id);
        }
    }
    None
}

/// Best-effort: send a `pw-cli destroy <id>` to remove the named
/// filter-chain node, in case the user wants a hard reset between
/// chain config changes. Errors are logged at debug level and
/// swallowed because the daemon shouldn't crash if the node is
/// already gone.
pub fn destroy_node_by_name(node_name: &str) {
    let id = match lookup_node_id(node_name) {
        Ok(id) => id,
        Err(e) => {
            debug!(node = node_name, error = %e, "destroy_node_by_name: not found");
            return;
        }
    };
    let res = Command::new("pw-cli")
        .args(["destroy", &id.to_string()])
        .output();
    match res {
        Ok(out) if !out.status.success() => {
            debug!(
                node = node_name,
                id,
                stderr = %String::from_utf8_lossy(&out.stderr),
                "pw-cli destroy failed"
            );
        }
        Err(e) => debug!(node = node_name, id, error = %e, "pw-cli destroy spawn failed"),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
id 35, type PipeWire:Interface:Node/3
    object.serial = \"71\"
    factory.id = \"6\"
    node.name = \"alsa_output.pci-0000_00_1f.3.analog-stereo\"
id 92, type PipeWire:Interface:Node/3
    object.serial = \"94\"
    node.name = \"ut-mic-processed\"
    media.class = \"Audio/Source\"
id 100, type PipeWire:Interface:Node/3
    node.name = \"firefox\"
";

    #[test]
    fn parse_node_id_finds_match() {
        assert_eq!(parse_node_id(SAMPLE, "ut-mic-processed"), Some(92));
        assert_eq!(parse_node_id(SAMPLE, "firefox"), Some(100));
    }

    #[test]
    fn parse_node_id_returns_none_for_missing() {
        assert_eq!(parse_node_id(SAMPLE, "nope"), None);
    }

    #[test]
    fn config_path_uses_xdg_or_home() {
        let p = config_path();
        let s = p.to_string_lossy();
        assert!(
            s.ends_with("pipewire/filter-chain.conf.d/50-undertone-mic.conf"),
            "unexpected: {s}"
        );
    }
}
