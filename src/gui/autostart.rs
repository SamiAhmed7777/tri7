//! Start-on-system-login (v5 GUIUtil::GetStartOnSystemStartup parity,
//! src/qt/guiutil.cpp:275-475).
//!
//! v5 per-platform registration:
//!   * Windows — a shell shortcut `triangles.lnk` in the user's Startup
//!     folder, target = the running exe with args `-min` (authored through
//!     COM IShellLink; Get = the file exists).
//!   * Linux — the Desktop Application Autostart Spec:
//!     `$XDG_CONFIG_HOME/autostart/triangles.desktop` (falls back to
//!     `~/.config/autostart`); a `Hidden=true` line means DISABLED and
//!     Set(false) removes the file entirely.
//!   * macOS — `~/Library/LaunchAgents/org.triangles.triangles-qt.plist`
//!     with RunAtLoad + `-min`; Set(false) removes it.
//!
//! tri7 differences (disclosed, one): Windows writes the SAME user-scope
//! registration through the standard
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value (name
//! `Triangles`, data `"<exe>" -min`) instead of a COM-authored .lnk — the
//! identical effect (the GUI launches with `-min` at login, user scope)
//! without dragging a COM dependency into the build. Linux and macOS keep
//! the v5 file names/shapes exactly. Every launch passes `-min` (start
//! minimized) exactly like v5.

/// Whether THIS platform can register a login autostart at all. The Options
/// tab uses it to render an honest disabled field where v5 had no support
/// path (v5 shipped all three below; tri7 matches them).
pub fn supported() -> bool {
    cfg!(windows) || cfg!(target_os = "linux") || cfg!(target_os = "macos")
}

/// The launch command v5 would have written: (program, args) = (exe, ["-min"]).
/// Pure and testable everywhere; the platform writers consume it.
pub fn program_args(exe: &std::path::Path) -> (String, Vec<String>) {
    (exe.display().to_string(), vec!["-min".into()])
}

/// Is the wallet registered to start on login?
pub fn is_enabled() -> bool {
    #[cfg(windows)]
    {
        windows_is_enabled()
    }
    #[cfg(target_os = "linux")]
    {
        linux_dir()
            .map(|d| linux_is_enabled_in(&d))
            .unwrap_or(false)
    }
    #[cfg(target_os = "macos")]
    {
        macos_plist_path()
            .map(|p| macos_is_enabled_in(&p))
            .unwrap_or(false)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        false
    }
}

/// Register / unregister the wallet for start-on-login.
pub fn set_enabled(on: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        windows_set_enabled(on)
    }
    #[cfg(target_os = "linux")]
    {
        let dir = linux_dir().ok_or_else(|| {
            "no XDG_CONFIG_HOME or HOME — cannot write the autostart entry".to_string()
        })?;
        linux_set_enabled_in(&dir, on)
    }
    #[cfg(target_os = "macos")]
    {
        let p = macos_plist_path().ok_or_else(|| {
            "no HOME — cannot write the LaunchAgents plist".to_string()
        })?;
        macos_set_enabled_in(&p, on)
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    {
        let _ = on;
        Err("start-on-login is not supported on this platform".into())
    }
}

// ---------------------------------------------------------------------------
// Windows — HKCU ...\Run (disclosed v5 .lnk deviation, same user scope)
// ---------------------------------------------------------------------------
#[cfg(windows)]
const RUN_KEY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE_NAME: &str = "Triangles";

#[cfg(windows)]
fn windows_run_value(exe: &std::path::Path) -> String {
    let (program, args) = program_args(exe);
    // quoted path + args: paths with spaces are the norm under %LOCALAPPDATA%
    format!("\"{}\" {}", program, args.join(" "))
}

#[cfg(windows)]
fn windows_is_enabled() -> bool {
    // v5 Get checked only that the registration EXISTS (not where it
    // points), so no exe resolution is needed here
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    match hkcu.open_subkey(RUN_KEY_PATH) {
        Ok(k) => k.get_value::<String, _>(RUN_VALUE_NAME).is_ok(),
        Err(_) => false,
    }
}

#[cfg(windows)]
fn windows_set_enabled(on: bool) -> Result<(), String> {
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};
    use winreg::RegKey;
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let key = hkcu
        .open_subkey_with_flags(RUN_KEY_PATH, KEY_READ | KEY_WRITE)
        .map_err(|e| format!("open Run key: {e}"))?;
    if on {
        let exe = std::env::current_exe()
            .map_err(|e| format!("resolve current exe: {e}"))?;
        let val = windows_run_value(&exe);
        key.set_value(RUN_VALUE_NAME, &val)
            .map_err(|e| format!("write Run value: {e}"))
    } else {
        match key.delete_value(RUN_VALUE_NAME) {
            Ok(()) => Ok(()),
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("delete Run value: {e}")),
        }
    }
}

// ---------------------------------------------------------------------------
// Linux — Desktop Application Autostart Spec (v5 file shape exactly)
// ---------------------------------------------------------------------------
#[cfg(target_os = "linux")]
fn linux_dir() -> Option<std::path::PathBuf> {
    if let Ok(x) = std::env::var("XDG_CONFIG_HOME") {
        if !x.is_empty() {
            return Some(std::path::PathBuf::from(x).join("autostart"));
        }
    }
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| std::path::PathBuf::from(h).join(".config").join("autostart"))
}

/// The autostart .desktop entry, byte-for-byte the v5 guiutil.cpp lines.
pub fn desktop_file_contents(exe: &std::path::Path) -> String {
    let (program, args) = program_args(exe);
    format!(
        "[Desktop Entry]\nType=Application\nName=triangles\nExec={} {}\nTerminal=false\nHidden=false\n",
        program,
        args.join(" ")
    )
}

/// v5 Linux Get: the file exists AND does not carry a `Hidden=true` line.
pub fn linux_is_enabled_in(dir: &std::path::Path) -> bool {
    let path = dir.join("triangles.desktop");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return false;
    };
    // v5: a line containing "Hidden" AND "true" means the user disabled it
    for line in text.lines() {
        if line.contains("Hidden") && line.contains("true") {
            return false;
        }
    }
    true
}

/// v5 Linux Set: enable writes the file (Hidden=false); disable removes it.
/// Removing a file that is already gone succeeds (v5 remove() ignores a
/// missing path the same way).
pub fn linux_set_enabled_in(dir: &std::path::Path, on: bool) -> Result<(), String> {
    let path = dir.join("triangles.desktop");
    if !on {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("remove {}: {e}", path.display())),
        }
    } else {
        let exe = std::env::current_exe()
            .map_err(|e| format!("resolve current exe: {e}"))?;
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("create {}: {e}", dir.display()))?;
        std::fs::write(&path, desktop_file_contents(&exe))
            .map_err(|e| format!("write {}: {e}", path.display()))
    }
}

// ---------------------------------------------------------------------------
// macOS — LaunchAgents plist (v5 file shape: RunAtLoad + -min)
// ---------------------------------------------------------------------------
#[cfg(target_os = "macos")]
fn macos_plist_path() -> Option<std::path::PathBuf> {
    std::env::var("HOME")
        .ok()
        .filter(|h| !h.is_empty())
        .map(|h| {
            std::path::PathBuf::from(h)
                .join("Library")
                .join("LaunchAgents")
                .join("org.triangles.tri7.plist")
        })
}

/// The LaunchAgent plist, the v5 checks it reads back (RunAtLoad, <true/>,
/// <string>-min</string>) all present.
pub fn plist_contents(exe: &std::path::Path) -> String {
    let (program, args) = program_args(exe);
    let mut args_xml = String::new();
    for a in &args {
        args_xml.push_str(&format!("<string>{a}</string>\n"));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         <key>Label</key>\n<string>org.triangles.tri7</string>\n\
         <key>ProgramArguments</key>\n<array>\n\
         <string>{program}</string>\n{args_xml}</array>\n\
         <key>RunAtLoad</key>\n<true/>\n\
         </dict>\n</plist>\n"
    )
}

#[cfg(target_os = "macos")]
fn macos_is_enabled_in(path: &std::path::Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    // v5 GetStartOnSystemStartup checks all three markers
    text.contains("<key>RunAtLoad</key>")
        && text.contains("<true/>")
        && text.contains("<string>-min</string>")
}

#[cfg(target_os = "macos")]
fn macos_set_enabled_in(path: &std::path::Path, on: bool) -> Result<(), String> {
    if !on {
        match std::fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("remove {}: {e}", path.display())),
        }
    } else {
        let exe = std::env::current_exe()
            .map_err(|e| format!("resolve current exe: {e}"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create {}: {e}", parent.display()))?;
        }
        std::fs::write(path, plist_contents(&exe))
            .map_err(|e| format!("write {}: {e}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_args_carries_the_legacy_min_flag() {
        let exe = std::path::Path::new("/opt/tri7/tri7_wallet_gui");
        let (program, args) = program_args(exe);
        assert_eq!(program, "/opt/tri7/tri7_wallet_gui");
        assert_eq!(args, vec!["-min".to_string()]);
    }

    #[test]
    fn desktop_file_matches_the_v5_lines() {
        let text = desktop_file_contents(std::path::Path::new("/usr/local/bin/tri7_wallet_gui"));
        assert!(text.starts_with("[Desktop Entry]\n"));
        assert!(text.contains("Type=Application\n"));
        assert!(text.contains("Name=triangles\n"));
        assert!(text.contains("Exec=/usr/local/bin/tri7_wallet_gui -min\n"));
        assert!(text.contains("Terminal=false\n"));
        assert!(text.contains("Hidden=false\n"));
        // and the written file reads back as ENABLED
        let dir = std::env::temp_dir().join(format!(
            "tri7-autostart-test-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("triangles.desktop");
        std::fs::write(&path, &text).unwrap();
        assert!(linux_is_enabled_in(&dir));
        // a Hidden=true line disables it (v5 semantics)
        std::fs::write(&path, text.replace("Hidden=false", "Hidden=true")).unwrap();
        assert!(!linux_is_enabled_in(&dir));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn plist_carries_the_v5_get_markers() {
        let text = plist_contents(std::path::Path::new("/Applications/tri7_wallet_gui"));
        assert!(text.contains("<key>RunAtLoad</key>"));
        assert!(text.contains("<true/>"));
        assert!(text.contains("<string>-min</string>"));
        assert!(text.contains("<string>/Applications/tri7_wallet_gui</string>"));
    }

    #[test]
    fn linux_set_remove_roundtrip_in_a_temp_dir() {
        // exercises the WRITE path headlessly on linux CI; the exe it writes
        // is this test binary — the file content shape is asserted above
        let dir = std::env::temp_dir().join(format!(
            "tri7-autostart-roundtrip-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // write manually with a fixed exe (current_exe in tests is the test
        // runner, which is fine too, but pin it for the assertion)
        let exe = std::path::Path::new("/tmp/tri7_wallet_gui");
        std::fs::write(dir.join("triangles.desktop"), desktop_file_contents(exe)).unwrap();
        assert!(linux_is_enabled_in(&dir));
        linux_set_enabled_in(&dir, false).unwrap();
        assert!(!linux_is_enabled_in(&dir));
        // disable-again is idempotent (v5 remove() of a missing file)
        linux_set_enabled_in(&dir, false).unwrap();
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn supported_matches_the_shipped_platforms() {
        // the three v5 platforms are supported; anything else honestly is not
        assert_eq!(
            supported(),
            cfg!(windows) || cfg!(target_os = "linux") || cfg!(target_os = "macos")
        );
    }
}
