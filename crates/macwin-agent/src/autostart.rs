//! Start at login: a LaunchAgent on macOS, the `Run` key of the current
//! user on Windows. Both start the tray (`macwin tray` inside MacWin.app,
//! `macwin-tray.exe`), which holds the instance lock, so a second copy
//! started by hand simply exits.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context;
use macwin_platform::fs::write_atomic;

/// The LaunchAgent label (also the bundle identifier of MacWin.app).
pub const LAUNCH_AGENT_LABEL: &str = "io.macwin.MacWin";
/// The `Run` key of the current user.
pub const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
/// The value name under [`RUN_KEY`].
pub const RUN_VALUE: &str = "macwin";
/// The tray executable next to `macwin.exe`.
pub const TRAY_EXE: &str = "macwin-tray.exe";

/// Where the login entry is kept.
pub trait AutostartStore: Send + Sync {
    /// The current entry, if any.
    fn read(&self) -> anyhow::Result<Option<String>>;
    fn write(&self, entry: &str) -> anyhow::Result<()>;
    /// Removes the entry; nothing to remove is fine.
    fn remove(&self) -> anyhow::Result<()>;
    /// Where the entry lives, for users.
    fn describe(&self) -> String;
}

/// A LaunchAgent property list (any directory, so tests use a temporary one).
pub struct LaunchAgentFile {
    pub path: PathBuf,
}

impl LaunchAgentFile {
    /// `~/Library/LaunchAgents/io.macwin.MacWin.plist` of `home`.
    pub fn in_home(home: &Path) -> LaunchAgentFile {
        LaunchAgentFile {
            path: home
                .join("Library/LaunchAgents")
                .join(format!("{LAUNCH_AGENT_LABEL}.plist")),
        }
    }
}

impl AutostartStore for LaunchAgentFile {
    fn read(&self) -> anyhow::Result<Option<String>> {
        match fs::read_to_string(&self.path) {
            Ok(text) => Ok(Some(text)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("cannot read {}", self.path.display())),
        }
    }

    fn write(&self, entry: &str) -> anyhow::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        // launchd refuses property lists that others can write: 0644.
        write_atomic(&self.path, entry.as_bytes(), false)
            .with_context(|| format!("cannot write {}", self.path.display()))
    }

    fn remove(&self) -> anyhow::Result<()> {
        match fs::remove_file(&self.path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                Err(e).with_context(|| format!("cannot remove {}", self.path.display()))
            }
            _ => Ok(()),
        }
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

/// A string value under `HKEY_CURRENT_USER` (tests use a scratch key).
#[cfg(windows)]
pub struct RunKey {
    pub subkey: String,
    pub name: String,
}

#[cfg(windows)]
impl AutostartStore for RunKey {
    fn read(&self) -> anyhow::Result<Option<String>> {
        Ok(macwin_platform_windows::registry::read_string(
            &self.subkey,
            &self.name,
        )?)
    }

    fn write(&self, entry: &str) -> anyhow::Result<()> {
        Ok(macwin_platform_windows::registry::write_string(
            &self.subkey,
            &self.name,
            entry,
        )?)
    }

    fn remove(&self) -> anyhow::Result<()> {
        Ok(macwin_platform_windows::registry::delete_value(
            &self.subkey,
            &self.name,
        )?)
    }

    fn describe(&self) -> String {
        format!(r"HKEY_CURRENT_USER\{} ({})", self.subkey, self.name)
    }
}

/// Whether this copy of macwin starts at login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutostartState {
    Off,
    On,
    /// An entry exists but starts something else (another copy of macwin,
    /// or one that has moved).
    Other,
}

/// The login entry of one executable in one store.
pub struct Autostart {
    store: Box<dyn AutostartStore>,
    entry: String,
    /// Entries older versions of this copy wrote.
    legacy: Vec<String>,
}

impl Autostart {
    pub fn new(store: Box<dyn AutostartStore>, entry: String) -> Autostart {
        Autostart {
            store,
            entry,
            legacy: Vec::new(),
        }
    }

    /// Entries an older version of this copy wrote, for [`Autostart::migrate`].
    pub fn with_legacy(mut self, legacy: Vec<String>) -> Autostart {
        self.legacy = legacy;
        self
    }

    /// Rewrites an entry an older version of this copy left (0.1 opened the
    /// window at login); entries of other copies stay. True when rewritten.
    pub fn migrate(&self) -> anyhow::Result<bool> {
        match self.store.read()? {
            Some(e) if self.legacy.contains(&e) => {
                self.store.write(&self.entry)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub fn state(&self) -> anyhow::Result<AutostartState> {
        Ok(match self.store.read()? {
            None => AutostartState::Off,
            Some(e) if e == self.entry => AutostartState::On,
            Some(_) => AutostartState::Other,
        })
    }

    /// Takes effect at the next login.
    pub fn enable(&self) -> anyhow::Result<()> {
        self.store.write(&self.entry)
    }

    pub fn disable(&self) -> anyhow::Result<()> {
        self.store.remove()
    }

    pub fn describe(&self) -> String {
        self.store.describe()
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The LaunchAgent that runs `exe tray --background` at login in GUI
/// sessions: the menu bar item without the window. No KeepAlive: quitting
/// must stick.
pub fn launch_agent_plist(exe: &Path) -> String {
    launch_agent_running(exe, &["tray", "--background"])
}

/// The LaunchAgent macwin 0.1 wrote, byte for byte: `exe tray`.
pub fn legacy_launch_agent_plist(exe: &Path) -> String {
    launch_agent_running(exe, &["tray"])
}

fn launch_agent_running(exe: &Path, args: &[&str]) -> String {
    let mut arguments = String::new();
    for a in std::iter::once(exe.display().to_string()).chain(args.iter().map(|a| a.to_string())) {
        arguments.push_str(&format!("\t\t<string>{}</string>\n", xml_escape(&a)));
    }
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>{LAUNCH_AGENT_LABEL}</string>
	<key>ProgramArguments</key>
	<array>
{arguments}	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#
    )
}

/// The `Run` value that starts `tray_exe` without its window (quoted:
/// paths contain spaces).
pub fn run_command(tray_exe: &Path) -> String {
    format!("{} --background", legacy_run_command(tray_exe))
}

/// The `Run` value macwin 0.1 wrote: `tray_exe` with no arguments.
pub fn legacy_run_command(tray_exe: &Path) -> String {
    format!("\"{}\"", tray_exe.display())
}

/// The app bundle `exe` lives in (`…/MacWin.app/Contents/MacOS/macwin`).
pub fn app_bundle(exe: &Path) -> Option<&Path> {
    let macos = exe.parent()?;
    let contents = macos.parent()?;
    let app = contents.parent()?;
    let is_bundle = macos.file_name()? == "MacOS"
        && contents.file_name()? == "Contents"
        && app.extension()? == "app";
    is_bundle.then_some(app)
}

/// The login entry of the running macwin on this machine.
pub fn for_this_machine() -> anyhow::Result<Autostart> {
    let exe = std::env::current_exe().context("cannot find the macwin executable")?;
    for_executable(&exe)
}

#[cfg(target_os = "macos")]
fn for_executable(exe: &Path) -> anyhow::Result<Autostart> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(Autostart::new(
        Box::new(LaunchAgentFile::in_home(Path::new(&home))),
        launch_agent_plist(exe),
    )
    .with_legacy(vec![legacy_launch_agent_plist(exe)]))
}

#[cfg(windows)]
fn for_executable(exe: &Path) -> anyhow::Result<Autostart> {
    let sibling = exe.with_file_name(TRAY_EXE);
    let target = if sibling.is_file() {
        sibling
    } else if exe.is_file() {
        exe.to_path_buf()
    } else {
        anyhow::bail!(
            "{} is missing next to {}; install macwin with install.ps1",
            TRAY_EXE,
            exe.display()
        );
    };
    Ok(Autostart::new(
        Box::new(RunKey {
            subkey: RUN_KEY.into(),
            name: RUN_VALUE.into(),
        }),
        run_command(&target),
    )
    .with_legacy(vec![legacy_run_command(&target)]))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn for_executable(_exe: &Path) -> anyhow::Result<Autostart> {
    anyhow::bail!("unsupported OS: macwin runs on macOS and Windows")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exercise(store: Box<dyn AutostartStore>, other: Box<dyn AutostartStore>, entry: &str) {
        let a = Autostart::new(store, entry.into());
        assert_eq!(a.state().unwrap(), AutostartState::Off);
        a.disable().unwrap();
        a.enable().unwrap();
        assert_eq!(a.state().unwrap(), AutostartState::On);
        a.enable().unwrap();
        assert_eq!(a.state().unwrap(), AutostartState::On);
        // Another copy sees this registration, then registers itself.
        let b = Autostart::new(other, format!("{entry}-other"));
        assert_eq!(b.state().unwrap(), AutostartState::Other);
        b.enable().unwrap();
        assert_eq!(a.state().unwrap(), AutostartState::Other);
        a.disable().unwrap();
        assert_eq!(a.state().unwrap(), AutostartState::Off);
        assert_eq!(b.state().unwrap(), AutostartState::Off);
    }

    #[test]
    fn launch_agent_files_switch_on_and_off() {
        let home = tempfile::tempdir().unwrap();
        let store = || Box::new(LaunchAgentFile::in_home(home.path()));
        let path = LaunchAgentFile::in_home(home.path()).path;
        assert!(path.ends_with("Library/LaunchAgents/io.macwin.MacWin.plist"));
        exercise(
            store(),
            store(),
            &launch_agent_plist(Path::new("/x/macwin")),
        );
        assert!(!path.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            store().write("x").unwrap();
            let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o644, "launchd ignores writable property lists");
        }
    }

    #[test]
    fn the_launch_agent_starts_the_tray_of_this_bundle() {
        let plist = launch_agent_plist(Path::new(
            "/Applications/Mac & Win <1>.app/Contents/MacOS/macwin",
        ));
        assert!(
            plist.contains("<string>io.macwin.MacWin</string>"),
            "{plist}"
        );
        assert!(
            plist.contains(
                "<string>/Applications/Mac &amp; Win &lt;1&gt;.app/Contents/MacOS/macwin</string>\n\t\t<string>tray</string>\n\t\t<string>--background</string>"
            ),
            "{plist}"
        );
        assert!(plist.contains("<key>RunAtLoad</key>\n\t<true/>"));
        assert!(!plist.contains("KeepAlive"), "quitting must stick");
    }

    /// What `launch_agent_plist` of macwin 0.1 wrote for `/x/macwin`.
    const PLIST_0_1: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>Label</key>
	<string>io.macwin.MacWin</string>
	<key>ProgramArguments</key>
	<array>
		<string>/x/macwin</string>
		<string>tray</string>
	</array>
	<key>RunAtLoad</key>
	<true/>
	<key>LimitLoadToSessionType</key>
	<string>Aqua</string>
	<key>ProcessType</key>
	<string>Interactive</string>
</dict>
</plist>
"#;

    #[test]
    fn a_login_item_of_version_0_1_opens_no_window_after_migrating() {
        let home = tempfile::tempdir().unwrap();
        let store = || Box::new(LaunchAgentFile::in_home(home.path()));
        let exe = Path::new("/x/macwin");
        assert_eq!(legacy_launch_agent_plist(exe), PLIST_0_1);
        let a = Autostart::new(store(), launch_agent_plist(exe))
            .with_legacy(vec![legacy_launch_agent_plist(exe)]);
        assert!(!a.migrate().unwrap(), "nothing to migrate");
        assert_eq!(a.state().unwrap(), AutostartState::Off);
        store().write(PLIST_0_1).unwrap();
        assert_eq!(a.state().unwrap(), AutostartState::Other);
        assert!(a.migrate().unwrap());
        assert_eq!(a.state().unwrap(), AutostartState::On);
        assert!(!a.migrate().unwrap(), "only once");
        // The entry of another copy stays.
        store()
            .write(&legacy_launch_agent_plist(Path::new("/y/macwin")))
            .unwrap();
        assert!(!a.migrate().unwrap());
        assert_eq!(a.state().unwrap(), AutostartState::Other);
    }

    #[test]
    fn run_commands_are_quoted() {
        assert_eq!(
            run_command(Path::new(r"C:\Users\A B\macwin-tray.exe")),
            r#""C:\Users\A B\macwin-tray.exe" --background"#
        );
        assert_eq!(
            legacy_run_command(Path::new(r"C:\Users\A B\macwin-tray.exe")),
            r#""C:\Users\A B\macwin-tray.exe""#,
            "as macwin 0.1 wrote it"
        );
    }

    #[test]
    fn app_bundles_are_recognised() {
        assert_eq!(
            app_bundle(Path::new("/Applications/MacWin.app/Contents/MacOS/macwin")),
            Some(Path::new("/Applications/MacWin.app"))
        );
        for not in [
            "/usr/local/bin/macwin",
            "/x/target/release/macwin",
            "/x/MacWin/Contents/MacOS/macwin",
            "macwin",
        ] {
            assert_eq!(app_bundle(Path::new(not)), None, "{not}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn run_key_values_switch_on_and_off() {
        let subkey = format!(r"Software\macwin-tests\autostart-{}", std::process::id());
        let store = || {
            Box::new(RunKey {
                subkey: subkey.clone(),
                name: RUN_VALUE.into(),
            })
        };
        exercise(
            store(),
            store(),
            &run_command(Path::new(r"C:\m\macwin-tray.exe")),
        );
        macwin_platform_windows::registry::delete_tree(&subkey).unwrap();
    }
}
