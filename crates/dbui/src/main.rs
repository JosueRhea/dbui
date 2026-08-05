//! Entry point.
//!
//! Deliberately empty: everything, including argument handling and the window,
//! belongs to a layer that can be tested. A `main` that does work is a `main`
//! that cannot be called from anywhere else.

fn main() {
    dbui_ui::run();
}
