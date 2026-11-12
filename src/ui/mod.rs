pub mod about;
pub mod app;
pub mod binary_view;
pub mod commands;
pub mod diffs_view;
pub mod folder_view;
pub mod fonts;
pub mod help;
pub mod icons;
pub mod image_view;
pub mod job_dialog;
#[cfg(target_os = "macos")]
mod macos;
pub mod markdown_view;
pub mod palette;
pub(crate) mod remote_folder;
pub mod sync_dialog;
pub mod syntax;
pub mod text_view;
pub mod theme;
pub mod web_view;
pub mod widgets;
pub mod xml_tree_view;

pub mod analysis;

pub mod baseline_panel;

pub mod merge_verification;
