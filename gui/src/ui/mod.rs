//! Drawing. `app.rs` is the shell (services, the engine read once a frame, the one
//! `dispatch`); `views/` lays out each mode from `panels/`; `widgets/` are the pieces
//! panels share; `modals/` the windows over the app; `theme.rs` the look; `prefs.rs`
//! what the UI keeps between runs; `flex.rs` how parts share the window (flexbox: see
//! `docs/ui-layout.md`), `layout.rs` the sizes they share; `window.rs` the window's size and the UI size.

pub mod app;
pub mod flex;
pub mod fonts;
pub mod layout;
pub mod modals;
pub mod panels;
pub mod prefs;
pub mod theme;
pub mod util;
pub mod views;
pub mod widgets;
pub mod window;
