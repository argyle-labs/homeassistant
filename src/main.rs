//! Dynamic (subprocess) entrypoint for the homeassistant plugin.
//!
//! Orca's boot-time scan finds this executable in its install dir, spawns it,
//! and speaks the UDS wire
//! protocol to it. The plugin is a `[[bin]]`, owns no runtime, and reaches orca
//! only through the socket.
//!
//! This is a **pure tool-surface** plugin — the manifest is the plugin's slice
//! of the linked `#[orca_tool]` inventory; there are no backends or schemas, and
//! no backend dispatch (mirroring the retired `export_tool_plugin!` with no
//! `backends`).
//!
//! It does NOT use the toolkit's `serve_tool_plugin!` pure arm: that arm derives
//! the namespace as `"{name}."` (`homeassistant.`), but this plugin's tool
//! namespace is the hyphenated `home-assistant.` (it deliberately does not match
//! the crate name — the retired export declared it explicitly via
//! `tool_prefixes: ["home-assistant."]`). No `serve_tool_plugin!` arm can express
//! a diverging prefix, so — like the `docker` plugin — the entry is hand-rolled
//! against [`serve`] to preserve the exact prefix. `target_compat` (`"2024.1+"`)
//! is carried as documentation, matching how the macro consumes it.

use plugin_toolkit::backend_def::{EMPTY_BACKENDS, EMPTY_SCHEMAS};
use plugin_toolkit::serve::{serve, PluginSpec};

fn main() -> plugin_toolkit::anyhow::Result<()> {
    // Force the linker to retain the lib rlib and its `#[orca_tool]` inventory;
    // without a reference from this bin the whole tool surface is dead-stripped.
    homeassistant::link_anchor();
    serve(PluginSpec {
        name: "homeassistant".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        // Explicit, hyphenated namespace — does NOT match the crate name.
        prefixes: vec!["home-assistant.".to_string()],
        backends_json: EMPTY_BACKENDS.to_string(),
        schema_json: EMPTY_SCHEMAS.to_string(),
        backend_dispatch: None,
    })
}
