//! Windows: embeds the app icon and version information, generated from the
//! crate version so Explorer and the updater agree with `--version`.

fn main() {
    println!("cargo:rerun-if-changed=../../assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let version = env!("CARGO_PKG_VERSION");
    let mut parts = version.split('.').map(|p| p.parse::<u16>().unwrap_or(0));
    let (major, minor, patch) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("manifest dir");
    let icon = std::path::Path::new(&manifest)
        .join("../../assets/icon.ico")
        .canonicalize()
        .expect("assets/icon.ico");
    // windres wants forward slashes in paths.
    let icon = icon.to_string_lossy().replace('\\', "/");
    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {major},{minor},{patch},0
PRODUCTVERSION {major},{minor},{patch},0
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "CompanyName", "Sebastian Schlosser"
      VALUE "FileDescription", "VNC Beach"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "vnc-beach"
      VALUE "LegalCopyright", "MIT License"
      VALUE "OriginalFilename", "vnc-beach.exe"
      VALUE "ProductName", "VNC Beach"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#
    );
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("out dir")).join("vnc-beach.rc");
    std::fs::write(&out, rc).expect("write vnc-beach.rc");
    embed_resource::compile(&out, embed_resource::NONE)
        .manifest_optional()
        .unwrap();
}
