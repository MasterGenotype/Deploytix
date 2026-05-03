fn main() {
    #[cfg(target_os = "linux")]
    {
        let out = std::env::var("OUT_DIR").unwrap();

        cc::Build::new()
            .file("src/resources/alsa_noop.c")
            .compile("alsa_noop");

        // cc::Build::compile emits rustc-link-lib + rustc-link-search, but in a
        // package with both a lib and a bin target Cargo sometimes propagates the
        // search path without the library name.  Passing the archive as a direct
        // linker argument is unconditional and avoids the propagation gap.
        println!("cargo:rustc-link-arg={out}/libalsa_noop.a");
        println!("cargo:rerun-if-changed=src/resources/alsa_noop.c");
    }
}
