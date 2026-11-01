//! KDE Plasma backend, doing what System Settings → Shortcuts does for a custom
//! command: a hidden desktop file plus its `_launch` action in kglobalaccel.
//! PowerDevil owns the brightness keys by default, so they move over to us and
//! go back to it on uninstall.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use zbus::blocking::Connection;

const POWERDEVIL: &str = "org_kde_powerdevil";
const POWERDEVIL_NAME: &str = "Power Management";
const LAUNCH_ACTION: &str = "_launch";
const BRIGHTNESS_STEP: i32 = 5;
/// kglobalaccel's `SetPresent | NoAutoloading`. Without `SetPresent` the keys are
/// stored but not grabbed until the next login, though `invokeShortcut` still works.
const SET_PRESENT_NO_AUTOLOADING: u32 = 2 | 4;

struct Binding {
    /// Desktop file name, which is also the kglobalaccel component name.
    file: &'static str,
    /// Dots are not allowed in D-Bus paths; kglobalaccel maps them to underscores.
    component_path: &'static str,
    name: &'static str,
    delta: i32,
    /// `Qt::Key_MonBrightnessUp` / `Qt::Key_MonBrightnessDown`.
    key: i32,
    powerdevil_action: &'static str,
}

const BINDINGS: [Binding; 2] = [
    Binding {
        file: "net.local.ddpm-brightness-up.desktop",
        component_path: "/component/net_local_ddpm_brightness_up_desktop",
        name: "Monitor brightness up (DDPM)",
        delta: BRIGHTNESS_STEP,
        key: 0x0100_00B2,
        powerdevil_action: "Increase Screen Brightness",
    },
    Binding {
        file: "net.local.ddpm-brightness-down.desktop",
        component_path: "/component/net_local_ddpm_brightness_down_desktop",
        name: "Monitor brightness down (DDPM)",
        delta: -BRIGHTNESS_STEP,
        key: 0x0100_00B3,
        powerdevil_action: "Decrease Screen Brightness",
    },
];

impl Binding {
    fn action_id(&self) -> Vec<&str> {
        vec![self.file, LAUNCH_ACTION, self.file, self.name]
    }

    fn powerdevil_id(&self) -> Vec<&str> {
        vec![
            POWERDEVIL,
            self.powerdevil_action,
            POWERDEVIL_NAME,
            self.powerdevil_action,
        ]
    }

    fn desktop_entry(&self, exe: &Path) -> String {
        format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name={}\n\
             Exec=\"{}\" brightness {:+}\n\
             NoDisplay=true\n\
             StartupNotify=false\n\
             X-KDE-GlobalAccel-CommandShortcut=true\n",
            self.name,
            exe.display(),
            self.delta
        )
    }
}

/// The kglobalaccel daemon's D-Bus interface.
struct GlobalAccel(Connection);

impl GlobalAccel {
    fn connect() -> Result<Self, String> {
        Connection::session()
            .map(Self)
            .map_err(|e| format!("cannot reach the session bus: {e}"))
    }

    fn call<B>(&self, method: &str, body: &B) -> Result<zbus::message::Message, String>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.0
            .call_method(
                Some("org.kde.kglobalaccel"),
                "/kglobalaccel",
                Some("org.kde.KGlobalAccel"),
                method,
                body,
            )
            .map_err(|e| format!("kglobalaccel {method}: {e}"))
    }

    fn set_keys(&self, action: Vec<&str>, keys: Vec<i32>) -> Result<(), String> {
        self.call("setForeignShortcut", &(action, keys)).map(drop)
    }

    /// Assign the binding's key and make it live in the current session.
    fn activate(&self, binding: &Binding) -> Result<(), String> {
        let flags = SET_PRESENT_NO_AUTOLOADING;
        self.call(
            "setShortcut",
            &(binding.action_id(), vec![binding.key], flags),
        )
        .map(drop)
    }

    fn is_active(&self, binding: &Binding) -> Result<bool, String> {
        self.0
            .call_method(
                Some("org.kde.kglobalaccel"),
                binding.component_path,
                Some("org.kde.kglobalaccel.Component"),
                "isActive",
                &(),
            )
            .and_then(|reply| reply.body().deserialize())
            .map_err(|e| format!("kglobalaccel isActive: {e}"))
    }

    fn keys(&self, action: Vec<&str>) -> Result<Vec<i32>, String> {
        self.call("shortcut", &(action,))?
            .body()
            .deserialize()
            .map_err(|e| format!("kglobalaccel shortcut: {e}"))
    }

    fn remove_component(&self, binding: &Binding) -> Result<(), String> {
        self.call("unregister", &(binding.file, LAUNCH_ACTION))?;
        // The component only exists while it has shortcuts, so this may find nothing.
        let _ = self.0.call_method(
            Some("org.kde.kglobalaccel"),
            binding.component_path,
            Some("org.kde.kglobalaccel.Component"),
            "cleanUp",
            &(),
        );
        Ok(())
    }
}

pub fn is_current_desktop() -> bool {
    std::env::var("XDG_CURRENT_DESKTOP")
        .is_ok_and(|desktops| desktops.split(':').any(|d| d.eq_ignore_ascii_case("KDE")))
}

fn applications_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .map(|data| data.join("applications"))
        .ok_or_else(|| "neither XDG_DATA_HOME nor HOME is set".to_string())
}

/// Make Plasma notice added or removed desktop files.
fn refresh_service_cache() {
    let refreshed = ["kbuildsycoca6", "kbuildsycoca5"].iter().any(|tool| {
        Command::new(tool)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    });
    if !refreshed {
        log::warn!("could not rebuild the KDE service cache; log out to apply the change");
    }
}

pub fn install() -> Result<(), String> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|e| format!("cannot locate the ddpm binary: {e}"))?;
    let dir = applications_dir()?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for binding in &BINDINGS {
        let path = dir.join(binding.file);
        std::fs::write(&path, binding.desktop_entry(&exe))
            .map_err(|e| format!("{}: {e}", path.display()))?;
    }
    refresh_service_cache();

    let accel = GlobalAccel::connect()?;
    for binding in &BINDINGS {
        // A key can only have one owner, so PowerDevil has to let go first.
        accel.set_keys(binding.powerdevil_id(), Vec::new())?;
        accel.call("doRegister", &(binding.action_id(),))?;
        accel.activate(binding)?;
        if accel.keys(binding.action_id())? != [binding.key] {
            return Err(format!(
                "{} is taken by another shortcut; free it in System Settings → Shortcuts",
                binding.name
            ));
        }
        if !accel.is_active(binding)? {
            return Err(format!("{} did not become active", binding.name));
        }
        println!(
            "{}: runs {} brightness {:+}",
            binding.name,
            exe.display(),
            binding.delta
        );
    }
    Ok(())
}

pub fn uninstall() -> Result<(), String> {
    let dir = applications_dir()?;
    let installed: Vec<_> = BINDINGS
        .iter()
        .filter(|b| dir.join(b.file).exists())
        .collect();
    if installed.is_empty() {
        println!("No ddpm shortcuts installed.");
        return Ok(());
    }
    let accel = GlobalAccel::connect()?;
    for binding in installed {
        accel.remove_component(binding)?;
        // Not `defaultShortcut`: kglobalaccel can forget it once the keys are cleared.
        accel.set_keys(binding.powerdevil_id(), vec![binding.key])?;
        if accel.keys(binding.powerdevil_id())? != [binding.key] {
            log::warn!("could not give {} back", binding.powerdevil_action);
        }
        let path = dir.join(binding.file);
        std::fs::remove_file(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("Removed {}", binding.name);
    }
    refresh_service_cache();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_entries_run_a_signed_step() {
        let up = BINDINGS[0].desktop_entry(Path::new("/opt/my apps/ddpm"));
        assert!(up.contains("Exec=\"/opt/my apps/ddpm\" brightness +5\n"));
        assert!(up.contains("X-KDE-GlobalAccel-CommandShortcut=true\n"));
        let down = BINDINGS[1].desktop_entry(Path::new("/opt/ddpm"));
        assert!(down.contains("brightness -5\n"));
    }

    #[test]
    fn component_paths_follow_kglobalaccel_naming() {
        for binding in &BINDINGS {
            let expected = binding.file.replace(['.', '-'], "_");
            assert_eq!(binding.component_path, format!("/component/{expected}"));
        }
    }
}
