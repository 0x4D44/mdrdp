//! Main-thread regression probe for MDR-BUG-FLUX-00003.
//!
//! Rust's standard test harness runs tests on worker threads, while AppKit and winit
//! require the process main thread. This harness-free target supplies that thread. The
//! window is opt-in because an ordinary `cargo test` must not flash UI; run it with
//! `MDRDP_GUI_TESTS=1 cargo test --test macos_menu_lifetime` on macOS.

#[cfg(not(target_os = "macos"))]
fn main() {}

#[cfg(target_os = "macos")]
fn main() {
    if std::env::var_os("MDRDP_GUI_TESTS").is_none() {
        return;
    }

    use dispatch2::DispatchQueue;
    use mdrdp::input::InputEvent;
    use mdrdp::surface::SurfaceStore;
    use mdrdp::wake::WakingSender;
    use mdrdp::window::{SessionWindow, WindowConfig};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::Duration;

    let event_loop = SessionWindow::event_loop().expect("create the one AppKit event loop");
    let (input_tx, _input_rx) = mpsc::channel::<InputEvent>();
    let window = SessionWindow::new(
        event_loop,
        WindowConfig::new("mdrdp menu lifetime probe", 64, 64),
        Arc::new(Mutex::new(SurfaceStore::new())),
        WakingSender::silent(input_tx),
    )
    .expect("create probe window");

    let waker = window.waker();
    let (observed_tx, observed_rx) = mpsc::channel();
    let closer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(100));
        DispatchQueue::main().exec_async(move || {
            let _ = observed_tx.send(session_menu_attached());
            let _ = waker.close();
        });
    });
    let _event_loop = window.run().expect("run and close the probe window");
    closer.join().expect("probe closer thread");
    assert!(
        observed_rx
            .recv()
            .expect("menu observation from main queue"),
        "the probe must observe the session menu before testing its teardown"
    );

    // winit may reinstall its own default menu as the loop stops. That is safe; the
    // defect is specifically leaving our `Session` menu attached after its Rust items
    // have been freed.
    assert!(
        !session_menu_attached(),
        "the ended session must detach its menu before SessionMenuBar drops"
    );
}

#[cfg(target_os = "macos")]
fn session_menu_attached() -> bool {
    use objc2::MainThreadMarker;
    use objc2_app_kit::NSApplication;

    let mtm = MainThreadMarker::new().expect("inspect AppKit only from its main thread");
    NSApplication::sharedApplication(mtm)
        .mainMenu()
        .is_some_and(|menu| {
            menu.itemArray()
                .iter()
                .any(|item| item.title().to_string() == "Session")
        })
}
