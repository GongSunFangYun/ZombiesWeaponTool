/// 编译期把软件信息嵌入 Windows VERSIONINFO 资源，
/// 显示在资源管理器 右键→属性→详细信息。
fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }
    let mut res = winres::WindowsResource::new();
    res.set("FileDescription", "ZombiesWeaponTool");
    res.set("ProductName", "ZombiesWeaponTool");
    res.set("ProductVersion", "1.2.1");
    res.set("FileVersion", "0.1.2.1");
    res.set("LegalCopyright", "© GongSunFangYun");
    res.set("CompanyName", "GongSunFangYun");
    res.set("OriginalFilename", "ZombiesWeaponTool.exe");
    res.set("InternalName", "ZombiesWeaponTool");
    res.set("Comments", "!?Zombies?!");
    // 语言：简体中文 (0x0804)
    res.set_language(0x0804);
    // 图标：项目目录下 app.ico
    let icon = std::env::var("CARGO_MANIFEST_DIR")
        .map(std::path::PathBuf::from)
        .unwrap()
        .join("app.ico");
    res.set_icon(icon.to_str().unwrap());
    if let Err(e) = res.compile() {
        // 缺 rc.exe/windres 时不阻断构建，仅警告
        println!("cargo:warning=Failed to embed the data: {e}");
    }
}
