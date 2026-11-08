//! The parts of OpenSSH's client configuration (`~/.ssh/config`, then `/etc/ssh/ssh_config`)
//! that say how to reach a server and sign in to it, so that an SFTP connection to an alias
//! such as `prod` finds the same server, user, keys and jump hosts `ssh prod` does.
//!
//! Read as `ssh` reads it: the first value found for a keyword is the one used (each
//! `IdentityFile` adds a key instead), `Host` and `Match` blocks apply when they match, and
//! `Include` reads other files in place. A `Match` line with a criterion this program can't
//! evaluate (`exec`, which would run a command, among them) doesn't match, negated or not.
//! Other keywords are passed over.

use crate::i18n::trf;
use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};

/// Where an SSH connection goes and how it signs in, once the configuration had its say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// The name the connection was asked for, an alias perhaps.
    pub alias: String,
    /// The server's real name (`HostName`), or the alias.
    pub host: String,
    pub port: u16,
    pub user: String,
    /// Private keys to try (`IdentityFile`), in order.
    pub identity_files: Vec<PathBuf>,
    /// Whether only `identity_files` are tried, not every key `ssh-agent` holds.
    pub identities_only: bool,
    /// Jump hosts to go through, in order (`ProxyJump`).
    pub jumps: Vec<Jump>,
    /// A command whose input and output carry the connection (`ProxyCommand`).
    pub proxy_command: Option<String>,
    /// The name the server's key is looked up under instead of its own (`HostKeyAlias`).
    pub host_key_alias: Option<String>,
    /// Known hosts files to check instead of `~/.ssh/known_hosts` (`UserKnownHostsFile`).
    pub known_hosts_files: Option<Vec<PathBuf>>,
}

/// A jump host of `ProxyJump`: `[user@]host[:port]`, each looked up in the configuration too.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Jump {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

/// The values found so far, unexpanded.
#[derive(Default)]
struct Found {
    host_name: Option<String>,
    user: Option<String>,
    port: Option<String>,
    identity_files: Vec<String>,
    identities_only: Option<bool>,
    /// `ProxyJump` or `ProxyCommand`, whichever came first: either one, `none` included,
    /// turns the other off.
    proxy: Option<Proxied>,
    host_key_alias: Option<String>,
    known_hosts_files: Option<Vec<String>>,
}

enum Proxied {
    Jump(String),
    Command(String),
}

/// How deep `Include`s may nest, as in OpenSSH.
const MAX_INCLUDE_DEPTH: usize = 16;

/// The configuration files read, most important first, each with the folder a relative
/// `Include` in it is in.
#[cfg(not(test))]
fn files() -> Vec<(PathBuf, PathBuf)> {
    let mut out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let ssh = home.join(".ssh");
        out.push((ssh.join("config"), ssh));
    }
    if cfg!(unix) {
        out.push((
            PathBuf::from("/etc/ssh/ssh_config"),
            PathBuf::from("/etc/ssh"),
        ));
    } else if let Some(data) = std::env::var_os("ProgramData") {
        let ssh = PathBuf::from(data).join("ssh");
        out.push((ssh.join("ssh_config"), ssh));
    }
    out
}

/// Only the file a test sets, never the user's own.
#[cfg(test)]
fn files() -> Vec<(PathBuf, PathBuf)> {
    CONFIG_PATH
        .lock()
        .ok()
        .and_then(|p| p.clone())
        .map(|p| {
            let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
            vec![(p, dir)]
        })
        .unwrap_or_default()
}

/// The configuration file a test reads instead, for every thread.
#[cfg(test)]
pub(crate) static CONFIG_PATH: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// The local user's name, as `%u` and the user to sign in as when nothing else names one.
pub fn local_user() -> String {
    ["USER", "USERNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|u| !u.is_empty()))
        .unwrap_or_default()
}

/// How a connection to `alias` goes. A `port` or `user` given wins over the configuration's,
/// as one given to `ssh` on its command line does; port 22 counts as not given.
pub fn lookup(alias: &str, port: Option<u16>, user: Option<&str>) -> Result<Resolved> {
    for name in std::iter::once(alias).chain(user) {
        if !valid_name(name) {
            bail!(trf(
                "msg.ssh_bad_name",
                &[("name", name.escape_debug().to_string())]
            ));
        }
    }
    let mut found = Found::default();
    let local = local_user();
    for (path, dir) in files() {
        let text = match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(e).context(trf(
                    "msg.ssh_config_unreadable",
                    &[("file", path.display().to_string())],
                ))
            }
        };
        let at = Reading {
            alias,
            user,
            local: &local,
            dir: &dir,
        };
        read(&text, &path, &at, true, 0, &mut found)?;
    }
    resolve(alias, port, user, &local, found)
}

/// Whether `name`, a server or a user name given to connect with, is safe to put into a
/// `ProxyCommand`, which the shell runs: as OpenSSH requires of the names given to it, nothing
/// the shell treats specially, no spaces or control characters, and no leading `-`.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('-')
        && !name
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "'`\"$\\;&<>|()".contains(c))
}

/// What a line is read against.
struct Reading<'a> {
    alias: &'a str,
    user: Option<&'a str>,
    local: &'a str,
    /// The folder a relative `Include` is in.
    dir: &'a Path,
}

/// The keyword of `line` (lowercase) and its arguments, as OpenSSH splits them: `keyword
/// value`, or `keyword=value`, with double quotes around an argument holding spaces.
fn split(line: &str) -> Option<(String, String)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let end = line
        .find(|c: char| c.is_whitespace() || c == '=')
        .unwrap_or(line.len());
    let (keyword, rest) = line.split_at(end);
    let rest = rest.trim_start();
    let rest = rest.strip_prefix('=').unwrap_or(rest).trim();
    Some((keyword.to_ascii_lowercase(), rest.to_string()))
}

/// The arguments of `rest`, split at spaces outside double quotes.
fn args(rest: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let (mut quoted, mut any) = (false, false);
    for c in rest.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any {
                    out.push(std::mem::take(&mut current));
                    any = false;
                }
            }
            c => {
                current.push(c);
                any = true;
            }
        }
    }
    if any {
        out.push(current);
    }
    out
}

/// Whether `name` (in any case) matches a comma-separated list of patterns: one of them
/// matching, and none negated with `!` matching.
fn matches_list(name: &str, list: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let mut matched = false;
    for pattern in list.split(',').filter(|p| !p.is_empty()) {
        let (negated, pattern) = match pattern.strip_prefix('!') {
            Some(p) => (true, p),
            None => (false, pattern),
        };
        if super::remote::glob_match(name.as_bytes(), pattern.to_ascii_lowercase().as_bytes()) {
            if negated {
                return false;
            }
            matched = true;
        }
    }
    matched
}

/// Whether a `Match` line's criteria all hold.
fn match_holds(criteria: &[String], at: &Reading, found: &Found) -> bool {
    let mut i = 0;
    while i < criteria.len() {
        let (negated, name) = match criteria[i].strip_prefix('!') {
            Some(n) => (true, n.to_ascii_lowercase()),
            None => (false, criteria[i].to_ascii_lowercase()),
        };
        i += 1;
        let holds = match name.as_str() {
            "all" => true,
            // This one reading is the last.
            "final" => true,
            "canonical" => false,
            "host" | "originalhost" | "user" | "localuser" => {
                let Some(list) = criteria.get(i) else {
                    return false;
                };
                i += 1;
                let value = match name.as_str() {
                    "host" => found.host_name.as_deref().unwrap_or(at.alias).to_string(),
                    "originalhost" => at.alias.to_string(),
                    "user" => at
                        .user
                        .map(str::to_string)
                        .or_else(|| found.user.clone())
                        .unwrap_or_else(|| at.local.to_string()),
                    _ => at.local.to_string(),
                };
                matches_list(&value, list)
            }
            // No tags are set here.
            "tagged" => {
                i += 1;
                false
            }
            // `exec` would run a command, and the others need what isn't known here: the line
            // can't be told to hold, whatever its other criteria or a `!` say.
            _ => return false,
        };
        if holds == negated {
            return false;
        }
    }
    true
}

/// Reads the configuration `text` (from `path`) into `found`, starting inside a block that
/// applies or not as `active` says.
fn read(
    text: &str,
    path: &Path,
    at: &Reading,
    mut active: bool,
    depth: usize,
    found: &mut Found,
) -> Result<()> {
    for line in text.lines() {
        let Some((keyword, rest)) = split(line) else {
            continue;
        };
        match keyword.as_str() {
            "host" => {
                let patterns = args(&rest);
                let mut matched = false;
                active = true;
                for p in &patterns {
                    match p.strip_prefix('!') {
                        Some(n) if matches_list(at.alias, n) => active = false,
                        Some(_) => {}
                        None => matched |= matches_list(at.alias, p),
                    }
                }
                active &= matched;
                continue;
            }
            "match" => {
                active = match_holds(&args(&rest), at, found);
                continue;
            }
            _ if !active => continue,
            "include" => {
                if depth >= MAX_INCLUDE_DEPTH {
                    bail!(trf(
                        "msg.ssh_config_unreadable",
                        &[("file", path.display().to_string())],
                    ));
                }
                for pattern in args(&rest) {
                    for file in include_files(&pattern, at.dir) {
                        let Ok(bytes) = std::fs::read(&file) else {
                            continue;
                        };
                        let text = String::from_utf8_lossy(&bytes);
                        read(&text, &file, at, true, depth + 1, found)?;
                    }
                }
                continue;
            }
            _ => {}
        }
        let first = args(&rest).into_iter().next();
        match keyword.as_str() {
            "hostname" => set(&mut found.host_name, first),
            "user" => set(&mut found.user, first),
            "port" => set(&mut found.port, first),
            "identityfile" => found.identity_files.extend(first),
            "identitiesonly" if found.identities_only.is_none() => {
                found.identities_only = first.map(|v| v.eq_ignore_ascii_case("yes"));
            }
            "proxyjump" if found.proxy.is_none() => {
                found.proxy = first.map(Proxied::Jump);
            }
            // A command line, kept as written.
            "proxycommand" if found.proxy.is_none() => {
                found.proxy = Some(Proxied::Command(rest.clone()));
            }
            "hostkeyalias" => set(&mut found.host_key_alias, first),
            "userknownhostsfile" if found.known_hosts_files.is_none() => {
                found.known_hosts_files = Some(args(&rest));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Keeps the first value found.
fn set(slot: &mut Option<String>, value: Option<String>) {
    if slot.is_none() {
        *slot = value;
    }
}

/// The files an `Include` of `pattern` names, relative to `dir` unless absolute, a `*` or `?`
/// in any part of the path matching any name there, in order of name.
fn include_files(pattern: &str, dir: &Path) -> Vec<PathBuf> {
    let path = tilde(pattern);
    let path = if path.is_absolute() {
        path
    } else {
        dir.join(path)
    };
    if !path.to_string_lossy().contains(['*', '?']) {
        return vec![path];
    }
    let mut found = vec![PathBuf::new()];
    for part in path.components() {
        let name = part.as_os_str().to_string_lossy();
        if !name.contains(['*', '?']) {
            found.iter_mut().for_each(|p| p.push(part));
            continue;
        }
        let mut next = Vec::new();
        for parent in &found {
            let mut here: Vec<PathBuf> = std::fs::read_dir(parent)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| {
                    super::remote::glob_match(
                        e.file_name().to_string_lossy().as_bytes(),
                        name.as_bytes(),
                    )
                })
                .map(|e| e.path())
                .collect();
            here.sort();
            next.extend(here);
        }
        found = next;
    }
    found.retain(|p| p.is_file());
    found
}

/// `path` with a leading `~` standing for the home folder.
fn tilde(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            let home = dirs::home_dir().unwrap_or_default();
            home.join(rest.trim_start_matches(['/', '\\']))
        }
        _ => PathBuf::from(path),
    }
}

/// `text` with `%` tokens expanded: `%h` the server's name, `%n` the alias, `%p` the port,
/// `%r` the user signed in as, `%u` the local user, `%d` the home folder, `%%` a `%`. Others
/// are kept as written.
fn expand(text: &str, r: &Resolved, local: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(&r.host),
            Some('n') => out.push_str(&r.alias),
            Some('p') => out.push_str(&r.port.to_string()),
            Some('r') => out.push_str(&r.user),
            Some('u') => out.push_str(local),
            Some('d') => out.push_str(&dirs::home_dir().unwrap_or_default().to_string_lossy()),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// A `HostName` value with its tokens expanded, the only two it may hold: `%h` the alias,
/// `%%` a `%`.
fn host_name(text: &str, alias: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('h') => out.push_str(alias),
            Some('%') => out.push('%'),
            Some(other) => {
                out.push('%');
                out.push(other);
            }
            None => out.push('%'),
        }
    }
    out
}

/// Reads a `ProxyJump` value: jump hosts separated by commas, each `[user@]host[:port]` or
/// `ssh://[user@]host[:port]`.
fn jumps(text: &str) -> Result<Vec<Jump>> {
    let bad = || trf("msg.ssh_config_bad_jump", &[("value", text.to_string())]);
    let mut out = Vec::new();
    for part in text.split(',') {
        let part = part.trim();
        let part = part.strip_prefix("ssh://").unwrap_or(part);
        let (user, host_port) = match part.rsplit_once('@') {
            Some((u, h)) => (Some(u.to_string()), h),
            None => (None, part),
        };
        let (host, port) = match host_port.strip_prefix('[') {
            Some(v6) => {
                let (host, after) = v6.split_once(']').with_context(bad)?;
                (host, after.strip_prefix(':'))
            }
            None => match host_port.rsplit_once(':') {
                Some((h, p)) => (h, Some(p)),
                None => (host_port, None),
            },
        };
        if host.is_empty() {
            bail!(bad());
        }
        let port = port
            .map(|p| p.parse().ok().filter(|&p: &u16| p > 0).with_context(bad))
            .transpose()?;
        out.push(Jump {
            user,
            host: host.to_string(),
            port,
        });
    }
    Ok(out)
}

/// The connection `found` describes, with what was given winning.
fn resolve(
    alias: &str,
    port: Option<u16>,
    user: Option<&str>,
    local: &str,
    found: Found,
) -> Result<Resolved> {
    let mut r = Resolved {
        alias: alias.to_string(),
        host: alias.to_string(),
        port: 22,
        user: local.to_string(),
        ..Resolved::default()
    };
    if let Some(name) = found.host_name.filter(|n| !n.is_empty()) {
        r.host = host_name(&name, alias);
    }
    r.port = match (port, found.port) {
        (Some(p), _) => p,
        (None, Some(p)) => p
            .parse()
            .ok()
            .filter(|&p: &u16| p > 0)
            .with_context(|| trf("msg.ssh_config_bad_port", &[("value", p.clone())]))?,
        (None, None) => 22,
    };
    if let Some(u) = user.map(str::to_string).or(found.user) {
        r.user = u;
    }
    r.identity_files = found
        .identity_files
        .iter()
        .filter(|f| !f.eq_ignore_ascii_case("none"))
        .map(|f| tilde(&expand(f, &r, local)))
        .collect();
    r.identities_only = found.identities_only.unwrap_or(false);
    match found.proxy {
        Some(Proxied::Jump(j)) if !j.eq_ignore_ascii_case("none") => r.jumps = jumps(&j)?,
        Some(Proxied::Command(c)) if !c.eq_ignore_ascii_case("none") && !c.is_empty() => {
            r.proxy_command = Some(expand(&c, &r, local));
        }
        _ => {}
    }
    r.host_key_alias = found.host_key_alias;
    r.known_hosts_files = found.known_hosts_files.map(|files| {
        files
            .iter()
            .filter(|f| !f.eq_ignore_ascii_case("none"))
            .map(|f| tilde(&expand(f, &r, local)))
            .collect()
    });
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_str(text: &str, alias: &str, user: Option<&str>) -> Resolved {
        let dir = tempfile::tempdir().unwrap();
        let at = Reading {
            alias,
            user,
            local: "me",
            dir: dir.path(),
        };
        let mut found = Found::default();
        read(text, Path::new("config"), &at, true, 0, &mut found).unwrap();
        resolve(alias, None, user, "me", found).unwrap()
    }

    #[test]
    fn the_first_value_found_for_a_host_is_the_one_used() {
        let text = "\
# a comment
Host prod prod-*
    HostName = prod.example.com
    User deploy
    Port 2200
    IdentityFile ~/.ssh/prod_%r_%h
    IdentitiesOnly yes

Host !prod-old *
    User fallback
    IdentityFile \"/keys/with space\"
    HostKeyAlias shared
    ProxyCommand nc -X 5 %h %p
";
        let r = read_str(text, "prod", None);
        assert_eq!((r.host.as_str(), r.port), ("prod.example.com", 2200));
        assert_eq!(r.user, "deploy");
        let home = dirs::home_dir().unwrap();
        assert_eq!(
            r.identity_files,
            [
                home.join(".ssh/prod_deploy_prod.example.com"),
                PathBuf::from("/keys/with space")
            ]
        );
        assert!(r.identities_only);
        assert_eq!(r.host_key_alias.as_deref(), Some("shared"));
        assert_eq!(
            r.proxy_command.as_deref(),
            Some("nc -X 5 prod.example.com 2200")
        );

        let r = read_str(text, "other", Some("given"));
        assert_eq!(
            (r.host.as_str(), r.port, r.user.as_str()),
            ("other", 22, "given")
        );
        let r = read_str(text, "prod-old", None);
        assert_eq!(r.user, "deploy", "the negated block is passed over");
        assert_eq!(r.proxy_command, None);
    }

    #[test]
    fn match_blocks_apply_by_host_and_user_and_never_run_commands() {
        let text = "\
Match host *.corp user ann
    ProxyJump ann@bastion.corp:2222,jump2
Match exec \"true\"
    Port 1
Match originalhost box !user root
    HostName box.internal
Match all
    Port 2022
";
        let r = read_str(text, "db.corp", Some("ann"));
        assert_eq!(
            r.jumps,
            [
                Jump {
                    user: Some("ann".into()),
                    host: "bastion.corp".into(),
                    port: Some(2222)
                },
                Jump {
                    user: None,
                    host: "jump2".into(),
                    port: None
                }
            ]
        );
        assert_eq!(r.port, 2022);
        assert!(read_str(text, "db.corp", Some("bob")).jumps.is_empty());
        assert_eq!(read_str(text, "box", Some("ann")).host, "box.internal");
        assert_eq!(read_str(text, "box", Some("root")).host, "box");
    }

    #[test]
    fn includes_are_read_in_place_and_none_turns_a_value_off() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("conf.d")).unwrap();
        std::fs::write(dir.path().join("conf.d/b.conf"), "Host *\n  Port 2\n").unwrap();
        std::fs::write(
            dir.path().join("conf.d/a.conf"),
            "Host web\n  HostName web.example\n  ProxyJump none\n",
        )
        .unwrap();
        let config = dir.path().join("config");
        std::fs::write(
            &config,
            "Host web\n  Include conf.d/*.conf\nHost *\n  ProxyJump jump\n  Port 3\n",
        )
        .unwrap();
        *CONFIG_PATH.lock().unwrap() = Some(config);
        let r = lookup("web", None, Some("ann")).unwrap();
        let other = lookup("other", Some(2222), None).unwrap();
        *CONFIG_PATH.lock().unwrap() = None;
        assert_eq!((r.host.as_str(), r.port), ("web.example", 2));
        assert!(r.jumps.is_empty(), "ProxyJump none came first");
        assert_eq!(other.port, 2222, "a port given wins");
        assert_eq!(other.jumps.len(), 1);
    }

    #[test]
    fn the_first_of_proxy_jump_and_proxy_command_turns_the_other_off() {
        let text = "\
Host *.corp
    ProxyJump bastion
Host internal
    ProxyCommand none
Host *
    ProxyCommand corp-connect %h %p
    ProxyJump other
";
        let r = read_str(text, "db.corp", None);
        assert_eq!(r.jumps.len(), 1);
        assert_eq!(r.proxy_command, None);
        let r = read_str(text, "internal", None);
        assert!(r.jumps.is_empty() && r.proxy_command.is_none(), "{r:?}");
        let r = read_str(text, "web", None);
        assert!(r.jumps.is_empty());
        assert_eq!(r.proxy_command.as_deref(), Some("corp-connect web 22"));
    }

    #[test]
    fn a_match_line_that_cant_be_told_to_hold_never_does_negated_or_not() {
        let text = "\
Match !exec \"on-vpn\"
    ProxyJump bastion
Match host web !localnetwork 10.0.0.0/8
    Port 1
Match !tagged x
    User tagless
Match !canonical
    HostName %h.example%%
";
        let r = read_str(text, "web", None);
        assert!(r.jumps.is_empty());
        assert_eq!(r.port, 22);
        assert_eq!(r.user, "tagless");
        assert_eq!(r.host, "web.example%");
        assert_eq!(host_name("a%%hb%h", "x"), "a%hbx");
    }

    #[test]
    fn names_the_shell_would_act_on_are_refused_before_any_command_runs() {
        for name in [
            "x;touch /tmp/p",
            "a`id`",
            "$(id)",
            "h|nc",
            "-oProxyCommand=x",
            "a b",
        ] {
            assert!(lookup(name, None, None).is_err(), "{name}");
            assert!(lookup("h", None, Some(name)).is_err(), "{name}");
        }
        assert!(lookup("h", None, Some("a\nb")).is_err());
        assert!(valid_name("host-1.example.com") && valid_name("[::1]") && valid_name("ann_b"));
    }

    #[test]
    fn includes_may_have_wildcards_in_folder_names_too() {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in [("a/config", "1"), ("b/config", "2"), ("b/other", "3")] {
            std::fs::create_dir_all(dir.path().join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.path().join(path), text).unwrap();
        }
        assert_eq!(
            include_files("*/config", dir.path()),
            [dir.path().join("a/config"), dir.path().join("b/config")]
        );
        assert_eq!(include_files("b/*", dir.path()).len(), 2);
        assert!(include_files("c*/config", dir.path()).is_empty());
    }

    #[test]
    fn bad_jump_hosts_and_ports_are_refused() {
        assert!(jumps("user@:22").is_err());
        assert!(jumps("h:0").is_err());
        assert_eq!(
            jumps("ssh://[::1]:2200").unwrap(),
            [Jump {
                user: None,
                host: "::1".into(),
                port: Some(2200)
            }]
        );
        let dir = tempfile::tempdir().unwrap();
        let at = Reading {
            alias: "h",
            user: None,
            local: "me",
            dir: dir.path(),
        };
        let mut found = Found::default();
        read("Port nope", Path::new("c"), &at, true, 0, &mut found).unwrap();
        assert!(resolve("h", None, None, "me", found).is_err());
    }
}
