//! The update chip in the status bar, and the state behind it.
//!
//! The policy lives here; the mechanism lives in [`dbui_app::updater`]. Nothing
//! installs itself: a check runs on launch, the download starts when the user
//! clicks, and the swap happens when they click again. One click per decision
//! they would want to make.

use dbui_app::updater::{self, Staged, Update};

use crate::root::{DbUi, Status};
use gpui::Context;

/// Where the update flow has got to. Only ever advances by a user click, apart
/// from the initial check.
#[derive(Debug, Default)]
pub enum UpdateState {
    /// Nothing to say -- either up to date, or this build cannot update itself.
    #[default]
    Idle,
    Checking,
    Available(Update),
    Downloading(Update),
    /// Downloaded and verified; waiting for the user to restart.
    Ready(Staged),
    /// Shown once, then clickable to try again.
    Failed(String),
}

impl DbUi {
    /// Ask GitHub whether there is anything newer, quietly.
    ///
    /// A failure here is deliberately not surfaced as an app error: the user
    /// did not ask, and a laptop that is merely offline should not open with a
    /// red status bar. It is kept in [`UpdateState::Failed`] so a manual retry
    /// can report it.
    pub(crate) fn check_for_update(&mut self, cx: &mut Context<Self>) {
        if !updater::is_updatable() || matches!(self.update, UpdateState::Checking) {
            return;
        }
        self.update = UpdateState::Checking;
        let task = updater::check(&self.runtime);
        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                this.update = match landed {
                    Some(Ok(Some(update))) => UpdateState::Available(update),
                    Some(Ok(None)) | None => UpdateState::Idle,
                    Some(Err(error)) => UpdateState::Failed(error.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Fetch the release the check found, verify it, and stage it.
    pub(crate) fn download_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Available(update) = &self.update else {
            return;
        };
        let update = update.clone();
        let task = updater::download(&self.runtime, update.clone());
        self.update = UpdateState::Downloading(update);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let landed = task.await;
            this.update(cx, |this, cx| {
                match landed {
                    Some(Ok(staged)) => this.update = UpdateState::Ready(staged),
                    Some(Err(error)) => {
                        // This one the user did ask for, so it is worth saying
                        // out loud rather than only in the chip.
                        this.status = Status::error(error.to_string());
                        this.update = UpdateState::Failed(error.to_string());
                    }
                    None => this.update = UpdateState::Idle,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Swap in the staged bundle and relaunch. Returns only if that failed.
    pub(crate) fn install_update(&mut self, cx: &mut Context<Self>) {
        let UpdateState::Ready(staged) = &self.update else {
            return;
        };
        // `install` returns `Result<Infallible, _>`: on success the process has
        // already been replaced, so the only way back here is an error.
        let Err(error) = updater::install(staged);
        self.status = Status::error(error.to_string());
        self.update = UpdateState::Failed(error.to_string());
        cx.notify();
    }

    /// What the status-bar chip says, and what clicking it does next.
    pub(crate) fn update_chip(&self) -> Option<(String, UpdateAction)> {
        match &self.update {
            UpdateState::Idle => None,
            UpdateState::Checking => None,
            UpdateState::Available(update) => Some((
                format!("Update to {} available", update.version),
                UpdateAction::Download,
            )),
            UpdateState::Downloading(update) => {
                Some((format!("Downloading {}…", update.version), UpdateAction::None))
            }
            UpdateState::Ready(staged) => Some((
                format!("Restart to update to {}", staged.version),
                UpdateAction::Install,
            )),
            // Say what went wrong, not just that something did -- "no network"
            // and "checksum mismatch" want very different reactions.
            UpdateState::Failed(reason) => Some((
                format!("Update failed: {} — retry", clip(reason, 64)),
                UpdateAction::Retry,
            )),
        }
    }
}

/// Trim to `max` characters on a char boundary, with an ellipsis if cut.
fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.lines().next().unwrap_or(text).chars().take(max).collect();
    if out.chars().count() < text.chars().count() {
        out.push('…');
    }
    out
}

/// What a click on the chip should do in the current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UpdateAction {
    None,
    Download,
    Install,
    Retry,
}
