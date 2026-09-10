//! Native GTK windows for settings and the dashboard.
//!
//! Linux only: GTK is a Linux-target dependency of this crate, and the tray
//! already owns a GTK main loop on the main thread, so these windows cost no
//! new dependency and no new system package. Every function here must be
//! called from that main thread.
//!
//! This module is deliberately thin. What the user sees is computed in
//! [`crate::view`] as plain data with tests; the code below only lays it out.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use gtk::prelude::*;
use serde_json::Value;

use crate::data::{Action, DataSource, Query};
use crate::view;

const PAD: i32 = 8;

thread_local! {
    /// One window each, reused. Clicking a menu item twice should raise the
    /// window that is already open, not stack a second copy behind it.
    static SETTINGS_WINDOW: RefCell<Option<gtk::Window>> = const { RefCell::new(None) };
    static DASHBOARD_WINDOW: RefCell<Option<gtk::Window>> = const { RefCell::new(None) };
}

fn present_existing(slot: &'static std::thread::LocalKey<RefCell<Option<gtk::Window>>>) -> bool {
    slot.with(|w| {
        if let Some(window) = w.borrow().as_ref() {
            window.present();
            return true;
        }
        false
    })
}

fn remember(
    slot: &'static std::thread::LocalKey<RefCell<Option<gtk::Window>>>,
    window: &gtk::Window,
) {
    slot.with(|w| *w.borrow_mut() = Some(window.clone()));
    window.connect_delete_event(move |_, _| {
        slot.with(|w| *w.borrow_mut() = None);
        gtk::glib::Propagation::Proceed
    });
}

fn window(title: &str, width: i32, height: i32) -> gtk::Window {
    let window = gtk::Window::new(gtk::WindowType::Toplevel);
    window.set_title(title);
    window.set_default_size(width, height);
    if let Ok(icon) = crate::icon::load_icon() {
        if let Some(pixbuf) = gtk::gdk_pixbuf::Pixbuf::from_mut_slice(
            icon.rgba,
            gtk::gdk_pixbuf::Colorspace::Rgb,
            true,
            8,
            icon.width as i32,
            icon.height as i32,
            icon.width as i32 * 4,
        )
        .into()
        {
            window.set_icon(Some(&pixbuf));
        }
    }
    window
}

fn vbox() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, PAD);
    b.set_margin_top(PAD);
    b.set_margin_bottom(PAD);
    b.set_margin_start(PAD);
    b.set_margin_end(PAD);
    b
}

/// A body label. Selectable, because paths and reasons are worth copying.
fn label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l.set_selectable(true);
    l
}

/// A notebook tab label. Deliberately NOT selectable.
///
/// A selectable GtkLabel owns an input window and takes button presses to
/// start a text selection, so as a tab label it swallows the click and the
/// notebook never switches page. Using the ordinary `label` helper here made
/// every tab in both windows completely dead while the rest of the window
/// worked, which is why tab labels get their own helper rather than a flag
/// someone can forget to pass.
fn tab(text: &str) -> gtk::Label {
    gtk::Label::new(Some(text))
}

fn markup(text: &str) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_markup(text);
    l.set_xalign(0.0);
    l
}

/// Delegates to the escaping in [`crate::view`], which is where the markup
/// strings are built and tested.
fn esc(text: &str) -> String {
    view::escape_markup(text)
}

fn scrolled(child: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Automatic)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .build();
    sw.add(child);
    sw
}

/// A heading + body pair used for every section of both windows.
fn section(title: &str) -> (gtk::Frame, gtk::Box) {
    let frame = gtk::Frame::new(Some(title));
    let body = vbox();
    frame.add(&body);
    (frame, body)
}

// ------------------------------------------------------------------ settings

pub fn open_settings_window(source: Arc<dyn DataSource>, config_path: PathBuf) {
    if present_existing(&SETTINGS_WINDOW) {
        return;
    }
    let win = window("Familiar — Settings", 720, 640);
    let notebook = gtk::Notebook::new();
    // Configuration first: the menu item is called Settings, and what a person
    // means by that is the things they can change, not a read-only status page.
    notebook.append_page(
        &config_tab(source.clone(), &win, &config_path),
        Some(&tab("Configuration")),
    );
    notebook.append_page(&inference_tab(source.clone()), Some(&tab("Inference")));
    win.add(&notebook);
    remember(&SETTINGS_WINDOW, &win);
    win.show_all();
}

fn inference_tab(source: Arc<dyn DataSource>) -> gtk::Widget {
    let root = vbox();
    root.pack_start(
        &markup("<small>Live state and connection tests.</small>"),
        false,
        false,
        0,
    );

    // The result of the most recent connection test, shared by every button.
    let result = label("");
    let body = vbox();

    match source.query(Query::InferenceStatus) {
        Ok(status) => {
            let v = view::build_settings_view(&status);
            body.pack_start(
                &markup(&format!("<b>Mode:</b> {}", esc(&v.text_mode))),
                false,
                false,
                0,
            );
            for (title, rows) in [
                ("Text Inference", &v.text),
                ("Embedding Inference", &v.embedding),
            ] {
                let (frame, inner) = section(title);
                if rows.is_empty() {
                    inner.pack_start(&markup("<i>not configured</i>"), false, false, 0);
                }
                for row in rows.iter() {
                    let line = gtk::Box::new(gtk::Orientation::Horizontal, PAD);
                    let name = if row.backend_name.is_empty() {
                        String::new()
                    } else {
                        format!(" <tt>{}</tt>", esc(&row.backend_name))
                    };
                    line.pack_start(
                        &markup(&format!(
                            "<b>{}:</b> {}{}",
                            esc(&row.caption),
                            esc(row.state.label()),
                            name
                        )),
                        false,
                        false,
                        0,
                    );
                    let test = gtk::Button::with_label("Test");
                    let source = source.clone();
                    let target = row.target.clone();
                    let result = result.clone();
                    test.connect_clicked(move |button| {
                        run_connection_test(source.clone(), &target, &result, button);
                    });
                    line.pack_end(&test, false, false, 0);
                    inner.pack_start(&line, false, false, 0);
                    if let Some(err) = &row.last_error {
                        inner.pack_start(
                            &markup(&format!("<small>{}</small>", esc(err))),
                            false,
                            false,
                            0,
                        );
                    }
                }
                body.pack_start(&frame, false, false, 0);
            }
        }
        Err(e) => body.pack_start(
            &label(&format!("Failed to read status: {e}")),
            false,
            false,
            0,
        ),
    }

    root.pack_start(&scrolled(&body), true, true, 0);
    root.pack_start(&result, false, false, 0);
    root.upcast()
}

/// A form over the global settings in config.toml.
///
/// Built from the document rather than from a hand-written list of settings,
/// so nothing global in the file is unreachable from here and the form does
/// not go stale as the config grows. Per-repository tables are deliberately
/// absent — they belong to a project and are edited on the project's page.
fn config_tab(
    source: Arc<dyn DataSource>,
    parent: &gtk::Window,
    config_path: &PathBuf,
) -> gtk::Widget {
    let document = match source.query(Query::ConfigDocument) {
        Ok(v) => v,
        Err(e) => return error_page("Configuration", &e),
    };
    let sections = view::build_config_form(document.get("document").unwrap_or(&Value::Null));
    let header = config_header(&document, config_path, None);
    config_form_widget(source, parent, sections, header, None)
}

/// The path of the file being edited, plus a way into a text editor for the
/// parts a form cannot express.
fn config_header(document: &Value, config_path: &PathBuf, note: Option<&str>) -> gtk::Box {
    let root = vbox();
    let line = gtk::Box::new(gtk::Orientation::Horizontal, PAD);
    let path_shown = document
        .get("path")
        .and_then(Value::as_str)
        .unwrap_or("config.toml")
        .to_string();
    line.pack_start(
        &markup(&format!("<tt><small>{}</small></tt>", esc(&path_shown))),
        false,
        false,
        0,
    );
    let open_file = gtk::Button::with_label("Open in editor");
    {
        let config_path = config_path.clone();
        open_file.connect_clicked(move |_| {
            if let Err(e) = opener::open(&config_path) {
                tracing::warn!(error = %e, "failed to open settings file");
            }
        });
    }
    line.pack_end(&open_file, false, false, 0);
    root.pack_start(&line, false, false, 0);
    if let Some(note) = note {
        root.pack_start(
            &markup(&format!("<small>{}</small>", esc(note))),
            false,
            false,
            0,
        );
    }
    root
}

/// One editor bound to one setting.
enum FieldWidget {
    Check(gtk::CheckButton),
    Entry(gtk::Entry),
    /// A closed set of options; the id carries the real value so the label may
    /// say more than the value does.
    Combo(gtk::ComboBoxText),
    /// Known options, but free text is still accepted.
    ComboEntry(gtk::ComboBoxText),
}

impl FieldWidget {
    fn current(&self) -> String {
        match self {
            Self::Check(check) => check.is_active().to_string(),
            Self::Entry(entry) => entry.text().to_string(),
            Self::Combo(combo) => combo.active_id().map(|s| s.to_string()).unwrap_or_default(),
            Self::ComboEntry(combo) => combo
                .child()
                .and_then(|child| child.downcast::<gtk::Entry>().ok())
                .map(|entry| entry.text().to_string())
                .unwrap_or_default(),
        }
    }
}

/// Builds the editor for one setting: a dropdown where the allowed values are
/// knowable, a checkbox for a flag, free text otherwise.
fn field_widget(field: &view::ConfigField, catalogue: &Value) -> FieldWidget {
    if let Some(set) = view::choices_for(&field.path, catalogue) {
        let (choices, open) = match set {
            view::ChoiceSet::Closed(choices) => (choices, false),
            view::ChoiceSet::Open(choices) => (choices, true),
        };
        let combo = if open {
            gtk::ComboBoxText::with_entry()
        } else {
            gtk::ComboBoxText::new()
        };
        // The value already in the file always remains selectable, even if it
        // is not one the catalogue knows about: the form must never silently
        // drop a setting the operator is using.
        if !open && !choices.iter().any(|c| c.value == field.value) && !field.value.is_empty() {
            combo.append(Some(&field.value), &format!("{} (current)", field.value));
        }
        for choice in &choices {
            combo.append(Some(&choice.value), &choice.label);
        }
        if open {
            if let Some(entry) = combo.child().and_then(|c| c.downcast::<gtk::Entry>().ok()) {
                entry.set_text(&field.value);
                entry.set_width_chars(28);
            }
            return FieldWidget::ComboEntry(combo);
        }
        combo.set_active_id(Some(&field.value));
        return FieldWidget::Combo(combo);
    }

    match field.kind {
        view::FieldKind::Bool => {
            let check = gtk::CheckButton::new();
            check.set_active(field.value == "true");
            FieldWidget::Check(check)
        }
        _ => {
            let entry = gtk::Entry::new();
            entry.set_text(&field.value);
            entry.set_hexpand(true);
            entry.set_width_chars(32);
            if field.kind == view::FieldKind::List {
                entry.set_placeholder_text(Some("comma-separated"));
            }
            FieldWidget::Entry(entry)
        }
    }
}

impl FieldWidget {
    fn as_widget(&self) -> gtk::Widget {
        match self {
            Self::Check(w) => w.clone().upcast(),
            Self::Entry(w) => w.clone().upcast(),
            Self::Combo(w) | Self::ComboEntry(w) => w.clone().upcast(),
        }
    }
}

/// Renders a set of config sections as an editable form and saves what changed.
fn config_form_widget(
    source: Arc<dyn DataSource>,
    parent: &gtk::Window,
    sections: Vec<view::ConfigSection>,
    header: gtk::Box,
    refresh: Option<Refresh>,
) -> gtk::Widget {
    // Asked once for the whole form: probing PATH and credentials per field
    // would be the same answer many times over.
    let catalogue = source.query(Query::ConfigChoices).unwrap_or(Value::Null);
    let root = vbox();
    root.pack_start(&header, false, false, 0);

    let body = vbox();
    let mut bound: Vec<(view::ConfigField, FieldWidget)> = Vec::new();
    // Every model dropdown on the form, so discovered models can be added to
    // all of them at once when the probe returns.
    let mut model_combos: Vec<gtk::ComboBoxText> = Vec::new();
    let known_models: Vec<String> = catalogue
        .get("models")
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    for sec in sections {
        let title = if sec.title.is_empty() {
            "(top level)".to_string()
        } else {
            sec.title.clone()
        };
        let (frame, inner) = section(&title);
        let grid = gtk::Grid::new();
        grid.set_column_spacing(PAD as u32);
        grid.set_row_spacing(4);
        for (row, field) in sec.fields.iter().enumerate() {
            let row = row as i32;
            // Saying where a value comes from is the point of the project
            // form: an inherited value looks identical to an overridden one
            // until it is labelled.
            let name = match field.origin {
                view::FieldOrigin::Global => esc(&field.name),
                view::FieldOrigin::Overridden => {
                    format!("<b>{}</b>  <small>overridden</small>", esc(&field.name))
                }
                view::FieldOrigin::Inherited => {
                    format!("{}  <small>inherited</small>", esc(&field.name))
                }
            };
            grid.attach(&markup(&name), 0, row, 1, 1);
            let widget = field_widget(field, &catalogue);
            if field.name == "model" {
                if let FieldWidget::ComboEntry(combo) = &widget {
                    model_combos.push(combo.clone());
                }
            }
            let w = widget.as_widget();
            w.set_hexpand(true);
            grid.attach(&w, 1, row, 1, 1);
            bound.push((field.clone(), widget));
        }
        inner.pack_start(&grid, false, false, 0);
        body.pack_start(&frame, false, false, 0);
    }

    root.pack_start(&scrolled(&body), true, true, 0);

    if !model_combos.is_empty() {
        discover_models_into(source.clone(), model_combos, known_models);
    }

    let save = gtk::Button::with_label("Save changes");
    {
        let source = source.clone();
        let parent = parent.clone();
        let bound = Rc::new(bound);
        save.connect_clicked(move |_| {
            // Only what actually changed is sent. Rewriting every field would
            // reformat settings the operator never touched — and on a project
            // form it would turn every inherited value into an override.
            let edits: Vec<crate::data::ConfigEdit> = bound
                .iter()
                .filter(|(field, widget)| widget.current() != field.value)
                .map(|(field, widget)| crate::data::ConfigEdit {
                    path: field.path.clone(),
                    value: widget.current(),
                })
                .collect();
            if edits.is_empty() {
                notify(&parent, gtk::MessageType::Info, "Nothing has changed.");
                return;
            }
            let refresh = refresh.clone().unwrap_or_else(|| Rc::new(RefCell::new(None)));
            run_action(
                source.clone(),
                Action::SaveConfig { edits },
                &refresh,
                &parent,
            );
        });
    }
    root.pack_start(&save, false, false, 0);
    root.upcast()
}

/// One project's settings: what it overrides, and what it inherits from the
/// global tables. Editing an inherited value creates the override.
fn project_settings_tab(
    source: Arc<dyn DataSource>,
    repo: &str,
    refresh: &Refresh,
    parent: &gtk::Window,
) -> gtk::Widget {
    let document = match source.query(Query::ConfigDocument) {
        Ok(v) => v,
        Err(e) => return error_page("Settings", &e),
    };
    let sections =
        view::build_project_config_form(document.get("document").unwrap_or(&Value::Null), repo);
    let config_path = PathBuf::from(
        document
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or("config.toml"),
    );
    let header = config_header(
        &document,
        &config_path,
        Some("Changing an inherited value creates an override for this project only."),
    );
    config_form_widget(source, parent, sections, header, Some(refresh.clone()))
}

/// Runs a connection probe off the GTK thread. This is the one query that
/// touches the network, and doing it inline would freeze the window for the
/// length of a timeout.
fn run_connection_test(
    source: Arc<dyn DataSource>,
    target: &str,
    result: &gtk::Label,
    button: &gtk::Button,
) {
    let (tx, rx) = mpsc::channel();
    let target_owned = target.to_string();
    std::thread::spawn(move || {
        let outcome = source.query(Query::TestConnection {
            target: target_owned,
        });
        let _ = tx.send(outcome);
    });

    result.set_text("Testing…");
    button.set_sensitive(false);
    let result = result.clone();
    let button = button.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || {
        match rx.try_recv() {
            Ok(outcome) => {
                button.set_sensitive(true);
                result.set_text(&describe_test(outcome));
                gtk::glib::ControlFlow::Break
            }
            Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
            Err(mpsc::TryRecvError::Disconnected) => {
                button.set_sensitive(true);
                result.set_text("Test failed: worker stopped");
                gtk::glib::ControlFlow::Break
            }
        }
    });
}

fn describe_test(outcome: Result<Value, String>) -> String {
    match outcome {
        Err(e) => format!("Test failed: {e}"),
        Ok(v) => {
            let status = v
                .get("status_text")
                .and_then(Value::as_str)
                .unwrap_or("no status reported");
            let latency = v
                .get("latency_ms")
                .and_then(Value::as_i64)
                .map(|ms| format!(" — {ms}ms"))
                .unwrap_or_default();
            let error = v
                .get("last_error")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .map(|e| format!(" ({e})"))
                .unwrap_or_default();
            format!("{status}{latency}{error}")
        }
    }
}

// ----------------------------------------------------------------- dashboard

/// A callback the action buttons use to redraw after they change something.
/// Held indirectly because the tabs are built by the very closure they need to
/// call, so it is filled in once that closure exists.
type Refresh = Rc<RefCell<Option<Rc<dyn Fn()>>>>;

fn refresh_now(refresh: &Refresh) {
    // Clone the callback out before invoking it: it rebuilds the tabs, which
    // borrows this same cell again.
    let callback = refresh.borrow().clone();
    if let Some(callback) = callback {
        callback();
    }
}

pub fn open_dashboard_window(source: Arc<dyn DataSource>) {
    if present_existing(&DASHBOARD_WINDOW) {
        return;
    }
    let win = window("Familiar", 980, 760);
    let root = vbox();

    let repos = source
        .query(Query::Repositories)
        .map(|v| view::build_repository_list(&v))
        .unwrap_or_default();

    let chooser = gtk::ComboBoxText::new();
    for repo in &repos {
        chooser.append_text(repo);
    }
    let notebook = gtk::Notebook::new();
    let state_label = markup("");
    let pause_button = gtk::Button::with_label("Pause project");

    // A wheel over a GtkComboBox changes its selection. Here that would
    // silently repoint every Start, Release and Force-complete button at a
    // different repository, so scrolling is refused on this control and the
    // selection is only ever changed deliberately.
    chooser.connect_scroll_event(|_, _| gtk::glib::Propagation::Stop);

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, PAD);
    bar.pack_start(&label("Repository:"), false, false, 0);
    bar.pack_start(&chooser, true, true, 0);
    bar.pack_start(&state_label, false, false, 0);
    bar.pack_end(&gtk::Label::new(None), false, false, 0);
    let refresh_button = gtk::Button::with_label("Refresh");
    bar.pack_end(&refresh_button, false, false, 0);
    bar.pack_end(&pause_button, false, false, 0);
    root.pack_start(&bar, false, false, 0);
    root.pack_start(&notebook, true, true, 0);

    if repos.is_empty() {
        root.pack_start(
            &label("No repository has stewardship state yet."),
            false,
            false,
            0,
        );
        pause_button.set_sensitive(false);
    } else {
        let selected = Rc::new(RefCell::new(repos[0].clone()));
        let refresh: Refresh = Rc::new(RefCell::new(None));

        let fill = {
            let notebook = notebook.clone();
            let source = source.clone();
            let selected = selected.clone();
            let refresh = refresh.clone();
            let state_label = state_label.clone();
            let pause_button = pause_button.clone();
            let win = win.clone();
            move || {
                let repo = selected.borrow().clone();
                let page = notebook.current_page();
                while notebook.n_pages() > 0 {
                    notebook.remove_page(Some(0));
                }
                notebook.append_page(
                    &gates_tab(source.clone(), &repo, &refresh, &win),
                    Some(&tab("Waiting on you")),
                );
                notebook.append_page(
                    &backlog_tab(source.clone(), &repo, &refresh, &win),
                    Some(&tab("Backlog")),
                );
                notebook.append_page(
                    &runs_tab(source.clone(), &repo, &refresh, &win),
                    Some(&tab("Runs")),
                );
                notebook.append_page(
                    &sessions_tab(source.clone(), &repo),
                    Some(&tab("Sessions")),
                );
                notebook.append_page(
                    &project_settings_tab(source.clone(), &repo, &refresh, &win),
                    Some(&tab("Settings")),
                );
                notebook.show_all();
                // Rebuilding the pages resets the selection; put the operator
                // back on the tab they were reading.
                if let Some(page) = page {
                    notebook.set_current_page(Some(page));
                }

                let state = source
                    .query(Query::ProjectState { repo: repo.clone() })
                    .map(|v| view::project_state_label(&v))
                    .unwrap_or_else(|e| format!("unknown ({e})"));
                state_label.set_markup(&format!("<small>project: {}</small>", esc(&state)));
                pause_button.set_label(if state == "paused" {
                    "Resume project"
                } else {
                    "Pause project"
                });
            }
        };
        let fill: Rc<dyn Fn()> = Rc::new(fill);
        *refresh.borrow_mut() = Some(fill.clone());

        // Select before connecting: setting the active row fires `changed`,
        // which would build every tab a second time on the way to the explicit
        // fill below.
        chooser.set_active(Some(0));
        {
            let refresh = refresh.clone();
            let selected = selected.clone();
            chooser.connect_changed(move |c| {
                if let Some(text) = c.active_text() {
                    *selected.borrow_mut() = text.to_string();
                    refresh_now(&refresh);
                }
            });
        }
        {
            let refresh = refresh.clone();
            refresh_button.connect_clicked(move |_| refresh_now(&refresh));
        }
        {
            let source = source.clone();
            let selected = selected.clone();
            let refresh = refresh.clone();
            let win = win.clone();
            pause_button.connect_clicked(move |button| {
                let paused = button.label().is_some_and(|l| l.starts_with("Pause"));
                run_action(
                    source.clone(),
                    Action::SetProjectPaused {
                        repo: selected.borrow().clone(),
                        paused,
                    },
                    &refresh,
                    &win,
                );
            });
        }
        fill();
    }

    win.add(&root);
    remember(&DASHBOARD_WINDOW, &win);
    win.show_all();
}

fn error_page(what: &str, e: &str) -> gtk::Widget {
    let b = vbox();
    b.pack_start(&label(&format!("{what} failed: {e}")), false, false, 0);
    b.upcast()
}

fn gates_tab(
    source: Arc<dyn DataSource>,
    repo: &str,
    refresh: &Refresh,
    parent: &gtk::Window,
) -> gtk::Widget {
    let value = match source.query(Query::Gates {
        repo: repo.to_string(),
    }) {
        Ok(v) => v,
        Err(e) => return error_page("Gates", &e),
    };
    let v = view::build_gates_view(&value);
    let reasons = source
        .query(Query::BlockedReasons {
            repo: repo.to_string(),
        })
        .map(|r| view::build_blocked_reasons(&r))
        .unwrap_or_default();
    let body = vbox();

    if v.groups.is_empty() {
        body.pack_start(&markup("<b>Nothing is blocked.</b>"), false, false, 0);
        return scrolled(&body).upcast();
    }

    body.pack_start(
        &markup(&format!(
            "<b>Waiting on you — {} PRD{}, {} stopped attempt{}</b>",
            v.groups.len(),
            if v.groups.len() == 1 { "" } else { "s" },
            v.stopped_attempts,
            if v.stopped_attempts == 1 { "" } else { "s" }
        )),
        false,
        false,
        0,
    );
    body.pack_start(
        &markup(
            "<small>Each of these has work sitting on a branch. Take it, re-drive it, \
             or throw it away.</small>",
        ),
        false,
        false,
        0,
    );

    let buttons_group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    for group in &v.groups {
        let entry = gtk::Box::new(gtk::Orientation::Vertical, 2);
        let line = gtk::Box::new(gtk::Orientation::Horizontal, PAD);
        line.pack_start(
            &markup(&format!(
                "<b>{}</b>  <small>{}</small>",
                esc(&group.prd_id),
                esc(&group.reasons.join(", "))
            )),
            false,
            false,
            0,
        );

        // The choices, as things to click. The commands are still available
        // below, but a person asking "what am I waiting to do?" needs the
        // options, not the syntax for typing them.
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, PAD / 2);
        buttons_group.add_widget(&actions);
        for (text, build) in [
            ("Re-drive", 0u8),
            ("Release", 1u8),
            ("Force-complete", 2u8),
        ] {
            let button = gtk::Button::with_label(text);
            button.set_tooltip_text(Some(match build {
                0 => "Run this PRD again from its retained candidate.",
                1 => "Return it to pending and discard the retained work.",
                _ => "Mark it completed without its gates being satisfied. Does not \
                      merge, deliver or verify anything.",
            }));
            let source = source.clone();
            let repo_owned = repo.to_string();
            let prd_id = group.prd_id.clone();
            let prd_path = group.prd_path.clone();
            let refresh = refresh.clone();
            let parent = parent.clone();
            button.connect_clicked(move |_| {
                let action = match build {
                    0 => Action::ResumePrd {
                        repo: repo_owned.clone(),
                        prd_id: prd_id.clone(),
                    },
                    1 => Action::ReleasePrd {
                        repo: repo_owned.clone(),
                        prd_path: prd_path.clone(),
                        actor: String::new(),
                        reason: String::new(),
                    },
                    _ => Action::CompletePrd {
                        repo: repo_owned.clone(),
                        prd_path: prd_path.clone(),
                        actor: String::new(),
                        reason: String::new(),
                    },
                };
                run_action(source.clone(), action, &refresh, &parent);
            });
            actions.pack_start(&button, false, false, 0);
        }
        let read = gtk::Button::with_label("Read");
        {
            let source = source.clone();
            let repo_owned = repo.to_string();
            let prd_path = group.prd_path.clone();
            let parent = parent.clone();
            read.connect_clicked(move |_| {
                show_prd_text(source.as_ref(), &repo_owned, &prd_path, &parent)
            });
        }
        actions.pack_start(&read, false, false, 0);
        line.pack_end(&actions, false, false, 0);
        entry.pack_start(&line, false, false, 0);

        let reason = reasons
            .iter()
            .find(|(path, _)| *path == group.prd_path)
            .map(|(_, r)| r);
        if let Some(reason) = reason {
            let what = markup(&format!("<small>{}</small>", esc(&reason.headline)));
            what.set_margin_start(PAD * 2);
            entry.pack_start(&what, false, false, 0);

            let outstanding = markup(&view::outstanding_markup(reason));
            outstanding.set_margin_start(PAD * 2);
            entry.pack_start(&outstanding, false, false, 0);

            if !reason.findings.is_empty() {
                let expander = gtk::Expander::new(None);
                expander.set_label_widget(Some(&markup("<small>which files</small>")));
                let detail = markup(&view::blocked_detail_markup(reason));
                detail.set_margin_start(PAD * 2);
                detail.set_selectable(true);
                expander.add(&detail);
                expander.set_margin_start(PAD * 2);
                entry.pack_start(&expander, false, false, 0);
            }
        }

        // Demoted, not removed: the commands are the audit trail and some
        // people would rather type. They are advice generated from the stop
        // reason, though, not from what is actually outstanding.
        if !group.recovery_commands.is_empty() {
            let expander = gtk::Expander::new(None);
            expander.set_label_widget(Some(&markup(
                "<small>equivalent commands</small>",
            )));
            let commands = markup(&format!(
                "<tt><small>{}</small></tt>",
                esc(&group.recovery_commands.join("\n"))
            ));
            commands.set_selectable(true);
            commands.set_margin_start(PAD * 2);
            expander.add(&commands);
            expander.set_margin_start(PAD * 2);
            entry.pack_start(&expander, false, false, 0);
        }

        entry.pack_start(
            &gtk::Separator::new(gtk::Orientation::Horizontal),
            false,
            false,
            0,
        );
        body.pack_start(&entry, false, false, 0);
    }
    scrolled(&body).upcast()
}

/// One backlog entry: its identity, its actions, and anything standing in its
/// way, kept together in a single container so the whole entry can be shown or
/// hidden as one thing when filtering.
#[allow(clippy::too_many_arguments)]
fn backlog_row(
    source: &Arc<dyn DataSource>,
    repo: &str,
    row: &view::BacklogRow,
    waiting_on: &[view::Blocker],
    progress: Option<&view::Progress>,
    blocked_reason: Option<&view::BlockedReason>,
    buttons_group: &gtk::SizeGroup,
    refresh: &Refresh,
    parent: &gtk::Window,
) -> gtk::Box {
    let container = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let line = gtk::Box::new(gtk::Orientation::Horizontal, PAD);
    line.pack_start(
        &markup(&format!(
            "<tt>{}</tt>  <small>{}</small>",
            esc(&row.prd_path),
            esc(&row.status)
        )),
        false,
        false,
        0,
    );

    // Buttons live in their own box of uniform width, so they line up in
    // columns instead of floating to a ragged right edge as the number of
    // available actions changes from row to row.
    let actions = gtk::Box::new(gtk::Orientation::Horizontal, PAD / 2);
    buttons_group.add_widget(&actions);

    let refusal = view::start_refusal(row, waiting_on);
    let start = gtk::Button::with_label("Start");
    if refusal.is_none() {
        let source = source.clone();
        let repo = repo.to_string();
        let prd_path = row.prd_path.clone();
        let refresh = refresh.clone();
        let parent = parent.clone();
        start.connect_clicked(move |_| {
            run_action(
                source.clone(),
                Action::StartPrd {
                    repo: repo.clone(),
                    prd_path: prd_path.clone(),
                },
                &refresh,
                &parent,
            );
        });
    } else if let Some(reason) = &refusal {
        // The runner would refuse this anyway — incomplete dependencies, or a
        // file that is no longer there. Disabled and labelled beats enabled
        // and futile.
        start.set_sensitive(false);
        start.set_tooltip_text(Some(reason));
    }

    if row.status == "in_progress" {
        for (text, release) in [("Release", true), ("Force-complete", false)] {
            let button = gtk::Button::with_label(text);
            button.set_tooltip_text(Some(if release {
                "Return this PRD to pending and discard the retained work."
            } else {
                "Mark this PRD completed in the backlog without its gates being \
                 satisfied. Does not merge, deliver or verify anything."
            }));
            let source = source.clone();
            let repo = repo.to_string();
            let prd_path = row.prd_path.clone();
            let refresh = refresh.clone();
            let parent = parent.clone();
            button.connect_clicked(move |_| {
                let action = if release {
                    Action::ReleasePrd {
                        repo: repo.clone(),
                        prd_path: prd_path.clone(),
                        actor: String::new(),
                        reason: String::new(),
                    }
                } else {
                    Action::CompletePrd {
                        repo: repo.clone(),
                        prd_path: prd_path.clone(),
                        actor: String::new(),
                        reason: String::new(),
                    }
                };
                run_action(source.clone(), action, &refresh, &parent);
            });
            actions.pack_start(&button, false, false, 0);
        }
    }
    actions.pack_start(&start, false, false, 0);

    let read = gtk::Button::with_label("Read");
    {
        let source = source.clone();
        let repo = repo.to_string();
        let prd_path = row.prd_path.clone();
        let parent = parent.clone();
        read.connect_clicked(move |_| show_prd_text(source.as_ref(), &repo, &prd_path, &parent));
    }
    actions.pack_start(&read, false, false, 0);
    line.pack_end(&actions, false, false, 0);
    container.pack_start(&line, false, false, 0);

    if let Some(reason) = &refusal {
        let note = markup(&view::refusal_markup(reason));
        note.set_margin_start(PAD * 2);
        container.pack_start(&note, false, false, 0);
    }

    if let Some(progress) = progress {
        let meter = markup(&view::progress_markup(progress));
        meter.set_margin_start(PAD * 2);
        container.pack_start(&meter, false, false, 0);

        // "blocked" alone is not actionable. Name the cause, and fold the
        // offending paths behind it so a thirty-finding stop stays readable.
        if progress.blocked {
            if let Some(reason) = blocked_reason {
                let expander = gtk::Expander::new(None);
                expander.set_label_widget(Some(&markup(&format!(
                    "<small>{}</small>",
                    esc(&reason.headline)
                ))));
                let detail = markup(&view::blocked_detail_markup(reason));
                detail.set_margin_start(PAD * 2);
                detail.set_selectable(true);
                expander.add(&detail);
                expander.set_margin_start(PAD * 3);
                container.pack_start(&expander, false, false, 0);
            }
        }
    }
    container.pack_start(
        &gtk::Separator::new(gtk::Orientation::Horizontal),
        false,
        false,
        0,
    );
    container
}

fn backlog_tab(
    source: Arc<dyn DataSource>,
    repo: &str,
    refresh: &Refresh,
    parent: &gtk::Window,
) -> gtk::Widget {
    let value = match source.query(Query::Backlog {
        repo: repo.to_string(),
        limit: 200,
    }) {
        Ok(v) => v,
        Err(e) => return error_page("Backlog", &e),
    };
    let v = view::build_backlog_view(&value);
    let progress = source
        .query(Query::Checkpoints {
            repo: repo.to_string(),
        })
        .map(|c| view::build_progress(&c))
        .unwrap_or_default();
    let blocked_reasons = source
        .query(Query::BlockedReasons {
            repo: repo.to_string(),
        })
        .map(|r| view::build_blocked_reasons(&r))
        .unwrap_or_default();
    let dependencies = source.query(Query::Dependencies {
        repo: repo.to_string(),
    });
    // A failed read must not look like "nothing is blocked". Say so, because
    // silently enabling every Start would be a wrong answer rather than a
    // missing one.
    let dependency_error = dependencies.as_ref().err().cloned();
    let blockers = dependencies
        .map(|d| view::build_blockers(&d))
        .unwrap_or_default();

    let root = vbox();
    let counts = v
        .counts
        .iter()
        .map(|(status, n)| format!("{}: {}", esc(status), n))
        .collect::<Vec<_>>()
        .join("   ");
    root.pack_start(&markup(&format!("<b>{counts}</b>")), false, false, 0);
    if v.truncated {
        root.pack_start(&markup("<small>first 200 shown</small>"), false, false, 0);
    }
    if let Some(error) = &dependency_error {
        root.pack_start(
            &markup(&format!(
                "<small>dependencies could not be read, so nothing is shown as \
                 blocked: {}</small>",
                esc(error)
            )),
            false,
            false,
            0,
        );
    }

    if v.open.is_empty() {
        root.pack_start(&label("Nothing open."), false, false, 0);
        return scrolled(&root).upcast();
    }

    let filter = gtk::SearchEntry::new();
    filter.set_placeholder_text(Some("filter by path"));
    root.pack_start(&filter, false, false, 0);

    let live = vbox();
    let stale = vbox();
    let buttons_group = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
    // Rows whose file still exists are the work; rows whose file has gone are
    // history the backlog has not caught up with. Interleaving them buried the
    // real work — most of this backlog is the second kind.
    let mut rows: Vec<(String, gtk::Box)> = Vec::new();
    let mut stale_count = 0usize;
    for row in &v.open {
        let waiting_on = blockers
            .iter()
            .find(|(path, _)| *path == row.prd_path)
            .map(|(_, list)| list.clone())
            .unwrap_or_default();
        let widget = backlog_row(
            &source,
            repo,
            row,
            &waiting_on,
            progress
                .iter()
                .find(|(path, _)| *path == row.prd_path)
                .map(|(_, p)| p),
            blocked_reasons
                .iter()
                .find(|(path, _)| *path == row.prd_path)
                .map(|(_, r)| r),
            &buttons_group,
            refresh,
            parent,
        );
        let target = if row.missing_since.is_some() {
            stale_count += 1;
            &stale
        } else {
            &live
        };
        target.pack_start(&widget, false, false, 0);
        rows.push((row.prd_path.clone(), widget));
    }

    root.pack_start(&live, false, false, 0);
    if stale_count > 0 {
        let expander = gtk::Expander::new(None);
        expander.set_label_widget(Some(&markup(&format!(
            "<b>{stale_count}</b> <small>entries whose file no longer exists — nothing \
             can be run from them</small>"
        ))));
        expander.add(&stale);
        expander.set_margin_top(PAD);
        root.pack_start(&expander, false, false, 0);
    }

    {
        // Filtering hides whole entries, notes and meter included, because the
        // pieces of one entry are meaningless apart from each other.
        let rows = rows.clone();
        filter.connect_search_changed(move |entry| {
            let needle = entry.text().to_lowercase();
            for (path, widget) in &rows {
                let matches = needle.is_empty() || path.to_lowercase().contains(&needle);
                // Without this, any later `show_all` on an ancestor would
                // resurrect every filtered-out entry.
                widget.set_no_show_all(!matches);
                widget.set_visible(matches);
            }
        });
    }

    scrolled(&root).upcast()
}

fn runs_tab(
    source: Arc<dyn DataSource>,
    repo: &str,
    refresh: &Refresh,
    parent: &gtk::Window,
) -> gtk::Widget {
    let value = match source.query(Query::Executions {
        repo: repo.to_string(),
        limit: 50,
    }) {
        Ok(v) => v,
        Err(e) => return error_page("Runs", &e),
    };
    let rows = view::build_executions_view(&value);
    let body = vbox();

    if rows.is_empty() {
        body.pack_start(
            &label("Nothing has been run through the control plane for this repository yet."),
            false,
            false,
            0,
        );
        return scrolled(&body).upcast();
    }

    for row in &rows {
        let line = gtk::Box::new(gtk::Orientation::Horizontal, PAD);
        line.pack_start(&markup(&view::execution_label_markup(row)), false, false, 0);
        if row.is_stoppable() {
            let stop = gtk::Button::with_label("Stop");
            let source = source.clone();
            let repo = repo.to_string();
            let execution_id = row.execution_id.clone();
            let refresh = refresh.clone();
            let parent = parent.clone();
            stop.connect_clicked(move |_| {
                run_action(
                    source.clone(),
                    Action::CancelExecution {
                        repo: repo.clone(),
                        execution_id: execution_id.clone(),
                    },
                    &refresh,
                    &parent,
                );
            });
            line.pack_end(&stop, false, false, 0);
        }
        body.pack_start(&line, false, false, 0);
    }
    scrolled(&body).upcast()
}

fn sessions_tab(source: Arc<dyn DataSource>, repo: &str) -> gtk::Widget {
    let value = match source.query(Query::Sessions {
        repo: repo.to_string(),
        limit: 10,
    }) {
        Ok(v) => v,
        Err(e) => return error_page("Sessions", &e),
    };
    let rows = view::build_sessions_view(&value);
    let body = vbox();

    if rows.is_empty() {
        body.pack_start(&label("No drive sessions yet."), false, false, 0);
        return scrolled(&body).upcast();
    }

    for row in rows {
        let expander = gtk::Expander::new(None);
        expander.set_label_widget(Some(&markup(&view::session_label_markup(&row))));

        // Detail is three more queries per session; only pay for the one the
        // user actually opens, and only once.
        let loaded = Rc::new(RefCell::new(false));
        let source = source.clone();
        let repo = repo.to_string();
        let session_id = row.session_id.clone();
        let placeholder = vbox();
        expander.add(&placeholder);
        expander.connect_expanded_notify(move |expander: &gtk::Expander| {
            if !expander.is_expanded() || *loaded.borrow() {
                return;
            }
            *loaded.borrow_mut() = true;
            if let Some(child) = expander.child() {
                expander.remove(&child);
            }
            let detail = session_detail_widget(&*source, &repo, &session_id);
            detail.set_margin_start(PAD * 2);
            expander.add(&detail);
            expander.show_all();
        });
        body.pack_start(&expander, false, false, 0);
    }
    scrolled(&body).upcast()
}

fn session_detail_widget(source: &dyn DataSource, repo: &str, session_id: &str) -> gtk::Box {
    let body = vbox();
    let budget = source.query(Query::Budget {
        repo: repo.to_string(),
        session_id: session_id.to_string(),
    });
    let attempts = source.query(Query::Attempts {
        repo: repo.to_string(),
        session_id: session_id.to_string(),
    });
    let review = source.query(Query::Review {
        repo: repo.to_string(),
        session_id: session_id.to_string(),
    });

    let (budget, attempts, review) = match (budget, attempts, review) {
        (Ok(b), Ok(a), Ok(r)) => (b, a, r),
        _ => {
            body.pack_start(&label("Could not read this session."), false, false, 0);
            return body;
        }
    };

    let v = view::build_session_detail(&budget, &attempts, &review);
    body.pack_start(&markup(&view::budget_markup(&v)), false, false, 0);

    let grid = gtk::Grid::new();
    grid.set_column_spacing(PAD as u32 * 2);
    for (col, heading) in ["#", "PRD", "Model", "Outcome", "Review", "Cost", "Took"]
        .iter()
        .enumerate()
    {
        grid.attach(&markup(&format!("<b>{heading}</b>")), col as i32, 0, 1, 1);
    }
    for (i, a) in v.attempts.iter().enumerate() {
        let line = i as i32 + 1;
        let outcome = view::attempt_outcome_markup(a);
        let review_cell = view::attempt_review_markup(a);
        grid.attach(&label(&a.sequence.to_string()), 0, line, 1, 1);
        grid.attach(&markup(&format!("<tt>{}</tt>", esc(&a.prd_id))), 1, line, 1, 1);
        grid.attach(&label(&a.model), 2, line, 1, 1);
        grid.attach(&markup(&outcome), 3, line, 1, 1);
        grid.attach(&markup(&review_cell), 4, line, 1, 1);
        grid.attach(&label(&a.cost), 5, line, 1, 1);
        grid.attach(&label(&a.duration), 6, line, 1, 1);
    }
    body.pack_start(&grid, false, false, 0);

    if !v.findings.is_empty() {
        body.pack_start(
            &markup("<b>Blocking review findings</b>"),
            false,
            false,
            0,
        );
        for f in &v.findings {
            body.pack_start(
                &markup(&format!(
                    "<tt>{}</tt> <b>{}</b> — <small>{}</small>\n<small><tt>{}</tt></small>",
                    esc(&f.prd_id),
                    esc(&f.rule),
                    esc(&f.detail),
                    esc(&f.path)
                )),
                false,
                false,
                0,
            );
        }
    }
    body
}

// -------------------------------------------------------------- actions

fn notify(parent: &gtk::Window, message_type: gtk::MessageType, text: &str) {
    let dialog = gtk::MessageDialog::new(
        Some(parent),
        gtk::DialogFlags::MODAL,
        message_type,
        gtk::ButtonsType::Ok,
        text,
    );
    dialog.run();
    dialog.close();
}

/// The actor recorded against a mutation. The backlog refuses anonymous
/// recovery, and `human:` is the prefix it requires for a person.
fn default_actor() -> String {
    format!(
        "human:{}",
        std::env::var("USER").unwrap_or_else(|_| "unknown".into())
    )
}

/// Confirms an action, collecting an actor and reason where the backlog
/// demands them. Returns `None` when the operator backs out — nothing has
/// happened at that point, because this runs before the action does.
fn confirm(parent: &gtk::Window, action: Action) -> Option<Action> {
    let needs_attribution = action.needs_actor_and_reason();
    let warning = action.warning();
    if warning.is_none() && !needs_attribution {
        return Some(action);
    }

    let dialog = gtk::Dialog::with_buttons(
        Some("Confirm"),
        Some(parent),
        gtk::DialogFlags::MODAL,
        &[
            ("Cancel", gtk::ResponseType::Cancel),
            ("Confirm", gtk::ResponseType::Accept),
        ],
    );
    let content = dialog.content_area();
    content.set_spacing(PAD);
    content.set_margin_top(PAD);
    content.set_margin_bottom(PAD);
    content.set_margin_start(PAD);
    content.set_margin_end(PAD);
    content.pack_start(
        &markup(&format!("<b>{}</b>", esc(&action.summary()))),
        false,
        false,
        0,
    );
    if let Some(warning) = warning {
        content.pack_start(&markup(&format!("<i>{}</i>", esc(warning))), false, false, 0);
    }

    let actor = gtk::Entry::new();
    let reason = gtk::Entry::new();
    if needs_attribution {
        actor.set_text(&default_actor());
        reason.set_placeholder_text(Some("why (recorded in the audit trail)"));
        let grid = gtk::Grid::new();
        grid.set_column_spacing(PAD as u32);
        grid.set_row_spacing(PAD as u32);
        grid.attach(&label("Actor"), 0, 0, 1, 1);
        grid.attach(&actor, 1, 0, 1, 1);
        grid.attach(&label("Reason"), 0, 1, 1, 1);
        grid.attach(&reason, 1, 1, 1, 1);
        actor.set_hexpand(true);
        reason.set_hexpand(true);
        content.pack_start(&grid, false, false, 0);

        // The mutation is refused without a reason, so do not let the operator
        // travel all the way to a rejection.
        if let Some(confirm) = dialog.widget_for_response(gtk::ResponseType::Accept) {
            confirm.set_sensitive(false);
            let actor_for_check = actor.clone();
            let reason_for_check = reason.clone();
            let check = move |confirm: &gtk::Widget| {
                confirm.set_sensitive(
                    !actor_for_check.text().trim().is_empty()
                        && !reason_for_check.text().trim().is_empty(),
                );
            };
            let c1 = confirm.clone();
            let check1 = check.clone();
            reason.connect_changed(move |_| check1(&c1));
            let c2 = confirm.clone();
            actor.connect_changed(move |_| check(&c2));
        }
    }

    dialog.show_all();
    let response = dialog.run();
    let (actor_text, reason_text) = (
        actor.text().trim().to_string(),
        reason.text().trim().to_string(),
    );
    dialog.close();
    if response != gtk::ResponseType::Accept {
        return None;
    }

    Some(match action {
        Action::ReleasePrd { repo, prd_path, .. } => Action::ReleasePrd {
            repo,
            prd_path,
            actor: actor_text,
            reason: reason_text,
        },
        Action::CompletePrd { repo, prd_path, .. } => Action::CompletePrd {
            repo,
            prd_path,
            actor: actor_text,
            reason: reason_text,
        },
        other => other,
    })
}

/// Confirms, then performs an action off the GTK thread, then redraws.
///
/// Off-thread because every action either writes the database or signals a
/// worker process, and a frozen window during a force-complete looks exactly
/// like a hung one.
fn run_action(
    source: Arc<dyn DataSource>,
    action: Action,
    refresh: &Refresh,
    parent: &gtk::Window,
) {
    let Some(action) = confirm(parent, action) else {
        return;
    };
    let summary = action.summary();
    let (tx, rx) = mpsc::channel();
    let worker_source = source.clone();
    std::thread::spawn(move || {
        let _ = tx.send(worker_source.act(action));
    });

    let refresh = refresh.clone();
    let parent = parent.clone();
    gtk::glib::timeout_add_local(Duration::from_millis(100), move || match rx.try_recv() {
        Ok(Ok(result)) => {
            // A successful action is normally silent — the redraw is the
            // feedback. It speaks up only when the daemon returns a `note`,
            // which it does for things the operator cannot see, such as a
            // setting that will not take effect until a restart.
            if let Some(note) = result.get("note").and_then(Value::as_str) {
                notify(&parent, gtk::MessageType::Info, note);
            }
            refresh_now(&refresh);
            gtk::glib::ControlFlow::Break
        }
        Ok(Err(e)) => {
            notify(
                &parent,
                gtk::MessageType::Error,
                &format!("{summary} failed.\n\n{e}"),
            );
            refresh_now(&refresh);
            gtk::glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => {
            notify(
                &parent,
                gtk::MessageType::Error,
                &format!("{summary} failed: worker stopped"),
            );
            gtk::glib::ControlFlow::Break
        }
    });
}

/// Shows one PRD's text. Read-only: this is for deciding, not editing.
fn show_prd_text(source: &dyn DataSource, repo: &str, prd_path: &str, parent: &gtk::Window) {
    let text = match source.query(Query::PrdText {
        repo: repo.to_string(),
        prd_path: prd_path.to_string(),
    }) {
        Ok(v) => v
            .get("text")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string(),
        Err(e) => {
            notify(
                parent,
                gtk::MessageType::Error,
                &format!("Could not read {prd_path}.\n\n{e}"),
            );
            return;
        }
    };

    let dialog = gtk::Dialog::with_buttons(
        Some(prd_path),
        Some(parent),
        gtk::DialogFlags::MODAL,
        &[("Close", gtk::ResponseType::Close)],
    );
    dialog.set_default_size(820, 700);
    let view = gtk::TextView::new();
    view.set_editable(false);
    view.set_monospace(true);
    view.set_left_margin(PAD);
    view.set_right_margin(PAD);
    view.buffer().map(|buffer| buffer.set_text(&text));
    let content = dialog.content_area();
    content.pack_start(&scrolled(&view), true, true, 0);
    dialog.show_all();
    dialog.run();
    dialog.close();
}


/// Asks the providers what models they serve, and adds anything new to the
/// model dropdowns once the answers arrive.
///
/// Off the GTK thread on purpose: this reaches out to every configured
/// provider over the network, and doing it while building the form would
/// freeze the window for as long as the slowest endpoint takes to answer.
fn discover_models_into(
    source: Arc<dyn DataSource>,
    combos: Vec<gtk::ComboBoxText>,
    known: Vec<String>,
) {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(source.query(Query::DiscoverModels));
    });

    gtk::glib::timeout_add_local(Duration::from_millis(150), move || match rx.try_recv() {
        Ok(Ok(result)) => {
            let discovered: Vec<String> = result
                .get("models")
                .and_then(Value::as_array)
                .map(|list| {
                    list.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            for model in discovered.iter().filter(|m| !known.contains(m)) {
                for combo in &combos {
                    combo.append(Some(model), model);
                }
            }
            if let Some(errors) = result.get("errors").and_then(Value::as_array) {
                for error in errors {
                    tracing::warn!(provider = %error, "model discovery failed for a provider");
                }
            }
            gtk::glib::ControlFlow::Break
        }
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "model discovery failed");
            gtk::glib::ControlFlow::Break
        }
        Err(mpsc::TryRecvError::Empty) => gtk::glib::ControlFlow::Continue,
        Err(mpsc::TryRecvError::Disconnected) => gtk::glib::ControlFlow::Break,
    });
}
