//! Warnings about completed operations that the library cannot print itself
//! (the TUI owns the terminal). Front ends drain and show them.

use std::sync::Mutex;

static WARNINGS: Mutex<Vec<String>> = Mutex::new(Vec::new());

pub(crate) fn push(message: String) {
    WARNINGS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .push(message);
}

/// Take every warning recorded since the last call.
pub fn take() -> Vec<String> {
    std::mem::take(
        &mut *WARNINGS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    )
}
