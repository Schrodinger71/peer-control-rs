fn main() {
    println!("cargo:rerun-if-env-changed=APP_VERSION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/packed-refs");

    // В CI релизный workflow задаёт APP_VERSION заранее (версия вычисляется
    // через git-cliff по conventional commits). Локально версия берётся
    // из `git describe`, чтобы dev-сборки тоже показывали осмысленную версию.
    let version = std::env::var("APP_VERSION").unwrap_or_else(|_| {
        std::process::Command::new("git")
            .args(["describe", "--tags", "--always", "--dirty"])
            .output()
            .ok()
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")))
    });
    println!("cargo:rustc-env=APP_VERSION={version}");

    #[cfg(target_os = "windows")]
    {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .set_manifest_file("assets/app.manifest")
            .compile()
            .expect("failed to embed the peer icon/manifest resources");
    }
}
