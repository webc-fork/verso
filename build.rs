fn main() {
    cfg_aliases::cfg_aliases! {
        // Platforms
        android: { target_os = "android" },
        macos: { target_os = "macos" },
        ios: { target_os = "ios" },
        apple: { any(target_os = "ios", target_os = "macos") },
        linux: { all(unix, not(apple), not(android)) },
    }

    // `cfg_aliases` only emits `--cfg`, so declare the aliases as expected here to keep
    // `unexpected_cfgs` enabled for them.
    for alias in ["android", "apple", "ios", "linux", "macos"] {
        println!("cargo::rustc-check-cfg=cfg({alias})");
    }

    #[cfg(all(feature = "packager", target_os = "macos"))]
    println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/../Resources/lib");
}
