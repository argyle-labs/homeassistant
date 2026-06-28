//! ABI-stable cdylib export for the homeassistant plugin.
//!
//! homeassistant is a **tool-surface** plugin, so its entire export is generated
//! by the toolkit's [`export_tool_plugin!`]: the metadata fns, the manifest
//! filtered from the linked `#[orca_tool]` inventory, the dispatch `invoke`, and
//! the empty `backends`/`schemas`. Its tool namespace is `home-assistant.`
//! (hyphenated — it does **not** match the `homeassistant` plugin name), so the
//! prefix is declared explicitly via `tool_prefixes`. The runtime singleton,
//! `minimal_ctx`, prefix filtering, and JSON encode/decode that used to live in
//! this file now live once in `plugin_toolkit::export`.
//!
//! `abi_stable` remains the crate's one direct non-orca dep because
//! `#[export_root_module]` (which the macro invokes) expands to bare
//! `::abi_stable` paths.

plugin_toolkit::export_tool_plugin! {
    name: "homeassistant",
    target_compat: "2024.1+",
    tool_prefixes: ["home-assistant."],
}
