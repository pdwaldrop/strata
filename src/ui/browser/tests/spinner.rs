// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;
use std::{
    cell::Cell,
    time::{Duration, Instant},
};

fn settle(millis: u64) {
    let deadline = Instant::now() + Duration::from_millis(millis);
    while Instant::now() < deadline {
        while glib::MainContext::default().iteration(false) {}
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Never finishes its own load until `released` is set, so a test can hold a
/// column in the `loading` state for as long as it needs.
struct GatedFileSource {
    released: Rc<Cell<bool>>,
}

impl crate::services::FileSource for GatedFileSource {
    fn validate_location(
        &self,
        _location: &Location,
    ) -> Result<(), crate::services::LocationValidationError> {
        Ok(())
    }

    fn enumerate(
        &self,
        request: crate::services::DirectoryRequest,
        emit: Rc<dyn Fn(crate::services::DirectoryEvent)>,
    ) -> crate::services::LoadHandle {
        let released = self.released.clone();
        let request_id = request.id;
        glib::idle_add_local(move || {
            if released.get() {
                emit(crate::services::DirectoryEvent::Finished {
                    request_id,
                    truncated: false,
                    can_trash: None,
                    can_delete: None,
                });
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        crate::services::LoadHandle::new(|| {})
    }
}

/// `rebuild_columns` (run every time the view switches into Columns mode) always
/// re-arms a delayed spinner via `append_column`, regardless of whether the
/// location it is rebuilding already finished loading. A location that is not
/// loading gets no further load event to stop that timer, so it must be
/// cancelled up front or it fires ~120ms later and spins forever.
#[test]
fn switching_to_columns_after_the_load_finished_does_not_leave_the_spinner_stuck() {
    const CHILD: &str = "STRATA_SPINNER_STUCK_GTK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let sandbox = tempfile::tempdir().expect("isolated settings");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "ui::browser::tests::spinner::switching_to_columns_after_the_load_finished_does_not_leave_the_spinner_stuck",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("XDG_CONFIG_HOME", sandbox.path().join("config"))
            .env("XDG_CACHE_HOME", sandbox.path().join("cache"))
            .env("XDG_DATA_HOME", sandbox.path().join("data"))
            .status()
            .expect("GTK test starts");
        assert!(status.success());
        return;
    }
    if gtk::init().is_err() {
        return;
    }
    crate::assets::prepare().expect("bundled assets");
    crate::assets::register_icon_theme();
    let fixture = tempfile::tempdir().expect("directory fixture");
    std::fs::write(fixture.path().join("file.txt"), "fixture").expect("fixture file");

    let view = BrowserView::new(
        Rc::new(crate::adapters::LocalFileSource),
        PeekBehavior::default(),
    );
    let browser = view.browser();
    view.set_view_mode(BrowserMode::Explorer);
    let window = gtk::Window::builder()
        .child(&view.widget())
        .default_width(640)
        .default_height(500)
        .build();
    window.present();
    browser.navigate(Location::local(fixture.path()));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !browser
        .column_snapshot(0)
        .is_some_and(|snapshot| !snapshot.loading)
    {
        assert!(Instant::now() < deadline, "directory load");
        glib::MainContext::default().iteration(false);
    }

    view.set_view_mode(BrowserMode::Columns);
    settle(250);

    let spinner = view.state.columns.borrow()[0].spinner.clone();
    assert!(
        !spinner.is_visible() && !spinner.is_spinning(),
        "the spinner must not fire after the fact for a location that already finished loading"
    );
}

/// The other side of the same fix: a location whose load is still genuinely in
/// flight when the view switches into Columns mode should show its spinner
/// immediately, not wait out the same delayed-arm timer meant for brand new
/// columns.
#[test]
fn switching_to_columns_while_still_loading_shows_the_spinner_immediately() {
    const CHILD: &str = "STRATA_SPINNER_LOADING_GTK_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let sandbox = tempfile::tempdir().expect("isolated settings");
        let status = std::process::Command::new(std::env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "ui::browser::tests::spinner::switching_to_columns_while_still_loading_shows_the_spinner_immediately",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("XDG_CONFIG_HOME", sandbox.path().join("config"))
            .env("XDG_CACHE_HOME", sandbox.path().join("cache"))
            .env("XDG_DATA_HOME", sandbox.path().join("data"))
            .status()
            .expect("GTK test starts");
        assert!(status.success());
        return;
    }
    if gtk::init().is_err() {
        return;
    }
    crate::assets::prepare().expect("bundled assets");
    crate::assets::register_icon_theme();

    let released = Rc::new(Cell::new(false));
    let view = BrowserView::new(
        Rc::new(GatedFileSource {
            released: released.clone(),
        }),
        PeekBehavior::default(),
    );
    let browser = view.browser();
    view.set_view_mode(BrowserMode::Explorer);
    let window = gtk::Window::builder()
        .child(&view.widget())
        .default_width(640)
        .default_height(500)
        .build();
    window.present();
    browser.navigate(Location::local("/"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !browser
        .column_snapshot(0)
        .is_some_and(|snapshot| snapshot.loading)
    {
        assert!(Instant::now() < deadline, "load should still be pending");
        glib::MainContext::default().iteration(false);
    }

    // Checked with no main-loop pumping at all: `set_visible`/`start` are plain
    // property writes that take effect synchronously, so a real fix needs no
    // iteration to observe. Pumping the loop here would blur this test with the
    // "already stuck" one above -- given enough wall-clock time (which even a
    // handful of dispatches can spend), the pre-fix delayed-arm timer fires on
    // its own and shows the spinner anyway, just 120ms late instead of never.
    view.set_view_mode(BrowserMode::Columns);

    let spinner = view.state.columns.borrow()[0].spinner.clone();
    assert!(
        spinner.is_visible() && spinner.is_spinning(),
        "a load already in flight must show its spinner right away, not 120ms later"
    );

    released.set(true);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !browser
        .column_snapshot(0)
        .is_some_and(|snapshot| !snapshot.loading)
    {
        assert!(
            Instant::now() < deadline,
            "gated load should finish once released"
        );
        glib::MainContext::default().iteration(false);
    }
    settle(50);
    assert!(
        !spinner.is_visible() && !spinner.is_spinning(),
        "the spinner should still stop normally once the load actually finishes"
    );
}
