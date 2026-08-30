/// Build-time resource embedding.
///
/// Injects Windows VERSIONINFO metadata into the binary so that the file's
/// "Properties → Details" tab (in Explorer) shows the product/company/version
/// info. This is purely cosmetic but improves the release artifact's polish.
fn main() {
    // Only relevant on Windows — the resource compiler is a Win32 tool.
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let mut res = winres::WindowsResource::new();
    res.set("FileDescription", "ZombiesWeaponTool");
    res.set("ProductName", "ZombiesWeaponTool");
    res.set("ProductVersion", "1.2.5");
    res.set("FileVersion", "0.1.2.5");
    res.set("LegalCopyright", "© GongSunFangYun");
    res.set("CompanyName", "GongSunFangYun");
    res.set("OriginalFilename", "ZombiesWeaponTool.exe");
    res.set("InternalName", "ZombiesWeaponTool");
    res.set("Comments", "!?Zombies?!");

    // VERSIONINFO language: Simplified Chinese (0x0804).
    res.set_language(0x0804);

    // Embed the application icon from `app.ico` in the crate root.
    let icon = std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap()
        .join("app.ico");
    res.set_icon(icon.to_str().unwrap());

    // The resource compiler (rc.exe/windres) is not always on PATH. If it's
    // missing, don't fail the build — a warning is enough (the binary just
    // ships without icon/metadata).
    if let Err(e) = res.compile() {
        println!("cargo:warning=Failed to embed the data: {e}");
    }
}
