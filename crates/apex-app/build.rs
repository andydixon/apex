//! Embeds the application icon and version resource into the Windows executable.
fn main() {
    println!("cargo:rerun-if-changed=../../assets/icons/apex.ico");
    #[cfg(windows)]
    {
        let ico = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons/apex.ico");
        if ico.exists() {
            let mut res = winresource::WindowsResource::new();
            res.set_icon(&ico.to_string_lossy());
            res.set("ProductName", "Apex");
            res.set("FileDescription", "Apex text editor");
            res.set("LegalCopyright", "AGPL-3.0-or-later");
            if let Err(e) = res.compile() {
                println!("cargo:warning=could not embed Windows resources: {e}");
            }
        } else {
            println!("cargo:warning=assets/icons/apex.ico missing; run packaging/icons/generate.sh");
        }
    }
}
