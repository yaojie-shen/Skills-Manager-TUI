//! Shared semantic presentation and rendering; pages only supply data and layout.

//! Shared semantic presentation. Pages pass content, selection and search state;
//! components own colours, markers, spacing and clipping across all densities.
//! Components must not import page modules or reload configuration from disk.
//! All runtime policies come from `Ctx.settings`; workspace tags/presets are data.

pub mod group;
pub mod group_prompt;
pub mod layout;
pub mod skill;

pub(crate) mod completion;
pub(crate) mod search_panel;

pub(crate) mod choice_footer;
pub(crate) mod command_palette;

pub(crate) mod context_menu;
