fn main() {
    println!("cargo:rerun-if-env-changed=APP_VERSION");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/packed-refs");

    // В CI релизный workflow задаёт APP_VERSION заранее (версия вычисляется
    // через git-cliff по conventional commits + короткий хэш коммита).
    // Локально версия собирается так же: последний тег + "-" + 6 символов
    // хэша HEAD, чтобы dev-сборки тоже показывали осмысленную версию.
    let version = std::env::var("APP_VERSION").unwrap_or_else(|_| {
        let run = |args: &[&str]| -> Option<String> {
            std::process::Command::new("git")
                .args(args)
                .output()
                .ok()
                .filter(|output| output.status.success())
                .and_then(|output| String::from_utf8(output.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        };

        let tag = run(&["describe", "--tags", "--abbrev=0"])
            .unwrap_or_else(|| format!("v{}", env!("CARGO_PKG_VERSION")));
        let short_sha = run(&["rev-parse", "--short=6", "HEAD"]);
        let dirty = run(&["status", "--porcelain"]).is_some();

        match short_sha {
            Some(sha) if dirty => format!("{tag}-{sha}-dirty"),
            Some(sha) => format!("{tag}-{sha}"),
            None => tag,
        }
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
