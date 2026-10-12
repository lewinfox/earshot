//! Drawing. `app.rs` is the shell (services, the engine read once a frame, the one
//! `dispatch`); `views/` lays out each mode from `panels/`; `widgets/` are the pieces
//! panels share; `modals/` the windows over the app; `theme.rs` the look; `prefs.rs`
//! what the UI keeps between runs.

pub mod app;
pub mod fonts;
pub mod modals;
pub mod panels;
pub mod prefs;
pub mod theme;
pub mod util;
pub mod views;
pub mod widgets;
