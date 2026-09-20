//! PRD-102: raising a flag when intervention is needed.
//!
//! A notification is a courtesy, not the record. The badge and the gates list
//! are read from durable state; this only tells the operator to go and look.
//! So a notifier that fails must never alter what the gate surface reports —
//! the same discipline the verification gate applies to unknown outcomes.

/// Somewhere to send a one-line "you are needed" message.
///
/// A trait rather than a direct call so the failure path is testable: a
/// desktop session is not always present, and a tray that misbehaves when the
/// notification service is missing is worse than one that stays quiet.
pub trait Notifier: Send + Sync {
    fn notify(&self, title: &str, body: &str) -> Result<(), String>;
}

/// The real one. `gio` arrives with the `gtk` dependency the tray already
/// carries, so notification costs no new dependency — the same constraint the
/// native settings and dashboard windows were built under.
#[cfg(target_os = "linux")]
pub struct DesktopNotifier {
    application_id: String,
}

#[cfg(target_os = "linux")]
impl DesktopNotifier {
    pub fn new(application_id: impl Into<String>) -> Self {
        Self {
            application_id: application_id.into(),
        }
    }
}

#[cfg(target_os = "linux")]
impl Notifier for DesktopNotifier {
    fn notify(&self, title: &str, body: &str) -> Result<(), String> {
        use gtk::gio::prelude::*;
        // An Application is the handle gio hangs notifications off. Registering
        // can fail when there is no session bus, which is a normal condition
        // on a headless box and must not be fatal.
        let app = gtk::gio::Application::new(
            Some(self.application_id.as_str()),
            gtk::gio::ApplicationFlags::FLAGS_NONE,
        );
        app.register(gtk::gio::Cancellable::NONE)
            .map_err(|error| format!("notification service unavailable: {error}"))?;
        let notification = gtk::gio::Notification::new(title);
        notification.set_body(Some(body));
        app.send_notification(None, &notification);
        Ok(())
    }
}

/// Drops everything. Used where no desktop session exists, and in tests that
/// assert the gate surface is unaffected by a notifier that does nothing.
pub struct SilentNotifier;

impl Notifier for SilentNotifier {
    fn notify(&self, _title: &str, _body: &str) -> Result<(), String> {
        Ok(())
    }
}
