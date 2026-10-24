//! The dual-pane browser: the panes, how they move together, and how their
//! listings are ordered.

pub mod pane;
pub mod sort;
pub mod sync_browse;
pub mod union;

pub use pane::{Pane, PaneState};
pub use sync_browse::{Followed, Side};
pub use union::{Origin, UnionRow};
