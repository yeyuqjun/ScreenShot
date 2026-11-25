fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    let target = std::env::var("TARGET").unwrap_or_default();
    if target.contains("windows-gnu") {
        let out_dir = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
        let rc = out_dir.join("snapdock.rc");
        let res = out_dir.join("snapdock.o");
        if let Ok(icon) = std::path::Path::new("../../assets/icon.ico").canonicalize() {
            let icon_str = icon
                .to_string_lossy()
                .trim_start_matches("\\\\?\\")
                .replace('\\', "/");
            if std::fs::write(&rc, format!("1 ICON \"{}\"\n", icon_str)).is_ok() {
                if let Ok(status) = std::process::Command::new("windres")
                    .args([
                        rc.to_str().unwrap_or_default(),
                        "-O",
                        "coff",
                        "-o",
                        res.to_str().unwrap_or_default(),
                    ])
                    .status()
                {
                    if status.success() {
                        println!("cargo:rustc-link-arg={}", res.display());
                    }
                }
            }
        }
    }
}
