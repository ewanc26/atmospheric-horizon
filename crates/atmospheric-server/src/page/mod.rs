//! The small HTML surface used by the browser link flow.

mod escape;
mod render;

pub(crate) use escape::html_escape;
pub(crate) use render::browser_page;
