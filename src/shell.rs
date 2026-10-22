//! Windows Explorer context menu integration ("Select Left" / "Compare To" / "Compare...").

#[cfg(windows)]
mod imp {
    use anyhow::Result;
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    const ROOTS: &[&str] = &["Software\\Classes\\*\\shell", "Software\\Classes\\Directory\\shell"];
    const ENTRIES: &[(&str, &str, &str)] = &[
        ("OxideMerge.SelectLeft", "shell.select_left", "--select-left"),
        ("OxideMerge.CompareTo", "shell.compare_to", "--compare-to"),
        ("OxideMerge.Open", "shell.compare", ""),
    ];

    pub fn register() -> Result<()> {
        let exe = std::env::current_exe()?;
        let exe = exe.display().to_string();
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for root in ROOTS {
            for (key, label, arg) in ENTRIES {
                let (k, _) = hkcu.create_subkey(format!("{root}\\{key}"))?;
                k.set_value("", &crate::i18n::tr(label).to_string())?;
                k.set_value("Icon", &exe)?;
                let (cmd, _) = k.create_subkey("command")?;
                let line = if arg.is_empty() {
                    format!("\"{exe}\" \"%1\"")
                } else {
                    format!("\"{exe}\" {arg} \"%1\"")
                };
                cmd.set_value("", &line)?;
            }
        }
        Ok(())
    }

    pub fn unregister() -> Result<()> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        for root in ROOTS {
            for (key, _, _) in ENTRIES {
                let _ = hkcu.delete_subkey_all(format!("{root}\\{key}"));
            }
        }
        Ok(())
    }

    pub fn is_registered() -> bool {
        RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey(format!("{}\\{}", ROOTS[0], ENTRIES[0].0))
            .is_ok()
    }
}

#[cfg(not(windows))]
mod imp {
    use anyhow::{bail, Result};

    pub fn register() -> Result<()> {
        bail!("shell integration is only available on Windows")
    }

    pub fn unregister() -> Result<()> {
        Ok(())
    }

    pub fn is_registered() -> bool {
        false
    }
}

pub use imp::*;
