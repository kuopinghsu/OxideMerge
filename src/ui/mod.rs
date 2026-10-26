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
#[cfg(target_os = "macos")]
mod macos;
pub mod palette;
pub mod syntax;
pub mod text_view;
pub mod theme;
pub mod web_view;
pub mod widgets;
