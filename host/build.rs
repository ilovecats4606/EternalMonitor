fn main() {
    println!("cargo:rerun-if-env-changed=ETERNAL_BUILD_REVISION");
    println!("cargo:rerun-if-changed=src");
    for args in [
        vec!["rev-parse", "--git-path", "HEAD"],
        vec!["rev-parse", "--git-path", "refs"],
    ] {
        if let Ok(output) = std::process::Command::new("git").args(args).output() {
            if output.status.success() {
                println!(
                    "cargo:rerun-if-changed={}",
                    String::from_utf8_lossy(&output.stdout).trim()
                );
            }
        }
    }
    let revision = std::env::var("ETERNAL_BUILD_REVISION").unwrap_or_else(|_| {
        let hash = std::process::Command::new("git")
            .args(["rev-parse", "--short=12", "HEAD"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let dirty = std::process::Command::new("git")
            .args(["diff", "--quiet", "HEAD"])
            .status()
            .is_ok_and(|status| !status.success());
        format!("{hash}{}", if dirty { "+dirty" } else { "" })
    });
    println!("cargo:rustc-env=ETERNAL_BUILD_REVISION={revision}");
    println!("cargo:rerun-if-changed=src/ffmpeg_log.c");
    println!("cargo:rerun-if-env-changed=FFMPEG_DIR");
    let mut native = cc::Build::new();
    native.file("src/ffmpeg_log.c");
    if let Some(root) = std::env::var_os("FFMPEG_DIR") {
        native.include(std::path::PathBuf::from(root).join("include"));
    } else {
        let library = pkg_config::Config::new()
            .probe("libavutil")
            .expect("libavutil headers");
        for path in library.include_paths {
            native.include(path);
        }
    }
    native.compile("eternal_ffmpeg_log");

    if std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default() == "windows" {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.compile().expect("Failed to compile Windows resources");
    }
}
