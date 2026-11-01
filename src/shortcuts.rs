//! `ddpm shortcuts install|uninstall`: bind the keyboard's brightness keys at
//! desktop level, so they reach ddpm while it isn't focused.
//!
//! - KDE Plasma: command shortcuts registered with kglobalaccel.
//! - elsewhere: [`install`] explains how to bind the keys by hand.

#[cfg(target_os = "linux")]
mod kde;

const MANUAL_HINT: &str = "automatic setup needs KDE Plasma; bind the brightness keys to \
                           `ddpm brightness +5` and `ddpm brightness -5` in your desktop's \
                           keyboard settings instead";

pub fn install() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if kde::is_current_desktop() {
        return kde::install();
    }
    Err(MANUAL_HINT.into())
}

/// Safe to run when nothing was installed.
pub fn uninstall() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    return kde::uninstall();
    #[cfg(not(target_os = "linux"))]
    {
        println!("Nothing to remove.");
        Ok(())
    }
}
