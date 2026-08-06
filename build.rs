fn main() {
    #[cfg(target_os = "windows")]
    {
        winresource::WindowsResource::new()
            .set_icon("assets/icon.ico")
            .set_manifest_file("assets/app.manifest")
            .compile()
            .expect("failed to embed the peer icon/manifest resources");
    }
}
