//! Dynamic (subprocess) entrypoint for the homeassistant plugin.
//!
//! Pure tool-surface plugin built on the typed [`Plugin`] builder. The plugin is
//! a `[[bin]]`, owns no runtime, and reaches orca only through the socket.
//!
//! The tool namespace is the hyphenated `home-assistant.` — it deliberately does
//! NOT match the crate/plugin name `homeassistant`, so the prefix is passed
//! explicitly to `.tools([...])`.
//!
//! `homeassistant::link_anchor()` force-links this plugin's own lib crate so its
//! `#[orca_tool]` inventory survives linking — without a reference from this bin
//! the whole tool surface is dead-stripped.
plugin_toolkit::instrument::bootstrap!();
use plugin_toolkit::plugin::Plugin;

fn main() -> plugin_toolkit::anyhow::Result<()> {
    homeassistant::link_anchor();
    Plugin::named("homeassistant")
        .version(env!("CARGO_PKG_VERSION"))
        .tools(["home-assistant."])
        .serve()
}
