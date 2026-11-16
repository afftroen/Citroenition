//! The in-app installer against the real release on GitHub (about 120 MB, so it is not part of a plain
//! `cargo test`): `cargo test -p citro-gui --test installer_net -- --ignored --nocapture`.

use citro_core::kernel::Kernel;
use citro_core::store::Home;
use citro_gui::backend::Backend;
use citro_gui::backend::local::LocalBackend;
use citro_gui::installer::CitroInstaller;
use std::sync::{Arc, Mutex};

#[test]
#[ignore = "downloads the real kernel from GitHub"]
fn the_in_app_installer_installs_the_real_kernel_and_the_second_run_is_a_no_op() {
    let root = std::env::temp_dir().join(format!("citro-gui-installer-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = Home { root: root.clone() };
    let app = LocalBackend::new(home.clone()).with_installer(Arc::new(CitroInstaller::default()));
    assert!(app.info().caps.kernel_install, "the button is enabled");
    assert!(app.kernel_status().unwrap().installed.is_none(), "nothing there yet");

    let seen = Mutex::new(Vec::<(u64, u64)>::new());
    let st = app.install_kernel(&|done, total| seen.lock().unwrap().push((done, total))).expect("install");
    let installed = st.installed.clone().expect("a kernel is active now");
    eprintln!("installed {} at {}", installed.version, installed.path);
    assert!(std::path::Path::new(&installed.path).is_file());
    assert_eq!(Kernel::discover(&home, None).unwrap().version, installed.version, "the launcher finds it too");
    assert_eq!(st.latest.as_deref(), Some(installed.version.as_str()), "and it is the newest one");
    assert!(!st.update_available());

    let seen = seen.into_inner().unwrap();
    assert_eq!(seen.first(), Some(&(0, 0)), "starts as 'looking it up'");
    let (done, total) = *seen.last().unwrap();
    assert!(total > 50 << 20 && done == total, "ends with the full download: {done}/{total}");
    assert!(seen.windows(2).all(|w| w[0].0 <= w[1].0), "the bar never goes back");

    // a second run finds it there and downloads nothing
    let again = Mutex::new(Vec::<(u64, u64)>::new());
    app.install_kernel(&|d, t| again.lock().unwrap().push((d, t))).expect("second install");
    assert!(again.into_inner().unwrap().iter().all(|&(d, _)| d == 0), "nothing was downloaded the second time");
    let _ = std::fs::remove_dir_all(&root);
}
