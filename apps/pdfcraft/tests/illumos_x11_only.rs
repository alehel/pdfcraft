#[test]
fn illumos_and_solaris_build_without_wayland() {
    // illumos and Solaris have no Wayland, and wayland-backend doesn't build there; winit and rfd
    // turn their Wayland support on for every Unix but macOS. The vendored copies leave it out
    // there (vendor/README.md). The cross check itself needs the target's standard library:
    // `cargo check -p pdfcraft --target x86_64-unknown-illumos` (or x86_64-pc-solaris).
    // This guards the copies, which workspace tests do not test themselves.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap().replace("\r\n", "\n");
    let solarish = r#"target_os = "illumos", target_os = "solaris""#;

    let lock = read("Cargo.lock");
    for name in ["winit", "rfd"] {
        let package = lock.split("[[package]]").find(|p| p.contains(&format!("\nname = \"{name}\"\n"))).expect("in Cargo.lock");
        assert!(!package.contains("\nsource = "), "{name} must resolve to vendor/{name}, not:{package}");
    }

    let winit = read("vendor/winit/Cargo.toml");
    for dep in ["sctk", "sctk-adwaita", "wayland-backend", "wayland-client", "wayland-protocols", "wayland-protocols-plasma"] {
        let table = winit.lines().find(|l| l.ends_with(&format!("'.dependencies.{dep}]"))).expect("winit dependency table");
        assert!(table.contains(solarish), "vendor/winit pulls {dep} in on illumos and Solaris: {table}");
    }
    let build = read("vendor/winit/build.rs");
    assert!(build.contains(
        r#"wayland_platform: { all(feature = "wayland", free_unix, not(redox), not(any(target_os = "illumos", target_os = "solaris"))) }"#
    ));
    let linux = read("vendor/winit/src/platform_impl/linux/mod.rs");
    assert!(
        linux.contains(r#"#[cfg(any(target_os = "netbsd", target_os = "illumos", target_os = "solaris"))]"#),
        "winit has no is_main_thread there"
    );

    let rfd = read("vendor/rfd/Cargo.toml");
    for dep in ["glib-sys", "gobject-sys", "gtk-sys", "libc", "percent-encoding", "pollster"] {
        let table = rfd.lines().find(|l| l.ends_with(&format!("'.dependencies.{dep}]"))).expect("rfd dependency table");
        assert!(table.contains(solarish), "vendor/rfd leaves {dep} out on illumos and Solaris: {table}");
    }
    for dep in ["wayland-backend", "wayland-client", "wayland-protocols"] {
        let table = rfd.lines().find(|l| l.ends_with(&format!("'.dependencies.{dep}]"))).expect("rfd dependency table");
        assert!(!table.contains("illumos"), "vendor/rfd pulls {dep} in on illumos: {table}");
    }
    let backend = read("vendor/rfd/src/backend.rs");
    for os in [r#"target_os = "illumos","#, r#"target_os = "solaris""#] {
        assert_eq!(backend.matches(os).count(), 3, "rfd has no dialog backend there");
    }
    let identifier = read("vendor/rfd/src/backend/xdg_desktop_portal/window_identifier/mod.rs");
    assert!(!identifier.contains(r#"#[cfg(feature = "wayland")]"#), "rfd uses Wayland there");

    // eframe's OpenGL renderer goes through glutin, which doesn't build there either.
    let app = read("apps/pdfcraft/Cargo.toml");
    assert!(app.contains("[target.'cfg(not(any(target_os = \"illumos\", target_os = \"solaris\")))'.dependencies]\n"));
    let eframe = app.lines().find(|l| l.starts_with("eframe = ")).expect("eframe dependency");
    assert!(!eframe.contains("\"glow\""), "glow is turned on everywhere: {eframe}");
}
