fn main() {
    tauri_build::try_build(
        tauri_build::Attributes::new().app_manifest(
            tauri_build::AppManifest::new().commands(&[
                "core_ping",
                "signer_ready",
                "bridge_deliver",
                "account",
                "shelf",
                "search_books",
                "book_detail",
                "directory",
                "chapter",
                "remember_book",
                "local_books",
                "set_progress",
                "win_min",
                "win_max_toggle",
                "win_close",
            ]),
        ),
    )
    .expect("failed to run tauri-build");
}
