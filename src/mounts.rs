//! Extra bind mounts for a Home Assistant deployment (media, backups, shares).
//!
//! A mount is `HOST:CONTAINER[:ro|:rw]`. Docker renders them into a Compose
//! override next to the base `compose.yml`; LXC renders `mpN` lines for the CT
//! plus the same override inside it, so the path is identical on the Proxmox
//! host side of the bind and inside the HA container.
#![allow(clippy::disallowed_types)]

use std::collections::BTreeSet;
use std::path::Path;

use plugin_toolkit::prelude::*;

/// Override file name, placed in the same directory as the base compose file.
pub const OVERRIDE_FILE: &str = "compose.mounts.yml";

/// Host trees an extra mount may come from. Anything else (`/`, `/etc`, `/root`)
/// would hand the container host secrets or system state.
pub const HOST_ROOTS: &[&str] = &["/mnt/", "/media/", "/srv/", "/data/"];

/// Container paths the base compose already binds; an extra mount over them
/// would shadow HA's own state or host integration.
const RESERVED: &[&str] = &["/config", "/etc/localtime", "/run/dbus"];

#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    plugin_toolkit::serde::Serialize,
    plugin_toolkit::serde::Deserialize,
    plugin_toolkit::schemars::JsonSchema,
)]
#[serde(crate = "plugin_toolkit::serde")]
#[schemars(crate = "plugin_toolkit::schemars")]
#[serde(rename_all = "camelCase")]
pub struct BindMount {
    pub host: String,
    pub container: String,
    pub read_only: bool,
}

fn check_path(kind: &str, p: &str) -> Result<()> {
    if !p.starts_with('/') {
        bail!("{kind} path '{p}' must be absolute");
    }
    // A strict charset keeps paths literal in the LXC `mpN:` syntax, the YAML
    // override, and provision.sh, so none of them needs escaping.
    if !p
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    {
        bail!("{kind} path '{p}' may only contain A-Z a-z 0-9 . _ / -");
    }
    // Raw segments: `Path::components` silently drops interior `.`.
    if p.split('/').any(|seg| seg == "." || seg == "..") {
        bail!("{kind} path '{p}' must not contain '.' or '..' components");
    }
    Ok(())
}

fn normalize(p: &str) -> String {
    let t = p.trim_end_matches('/');
    if t.is_empty() {
        "/".to_string()
    } else {
        t.to_string()
    }
}

/// Parses one `HOST:CONTAINER[:ro|:rw]` spec.
pub fn parse(spec: &str) -> Result<BindMount> {
    let parts: Vec<&str> = spec.split(':').collect();
    let (host, container, read_only) = match parts.as_slice() {
        [h, c] => (*h, *c, false),
        [h, c, "ro"] => (*h, *c, true),
        [h, c, "rw"] => (*h, *c, false),
        [_, _, m] => bail!("mount '{spec}': mode must be 'ro' or 'rw', got '{m}'"),
        _ => bail!("mount '{spec}' must be HOST:CONTAINER[:ro|:rw]"),
    };
    check_path("host", host)?;
    check_path("container", container)?;
    let host_norm = format!("{}/", normalize(host));
    if !HOST_ROOTS
        .iter()
        .any(|r| host_norm.starts_with(r) && host_norm.len() > r.len())
    {
        bail!(
            "host path '{host}' must be under one of {}",
            HOST_ROOTS.join(", ")
        );
    }
    let container = normalize(container);
    if container == "/" {
        bail!("mount '{spec}' must not target the container root");
    }
    if RESERVED
        .iter()
        .any(|r| container == *r || container.starts_with(&format!("{r}/")))
    {
        bail!("mount '{spec}' targets reserved path '{container}'");
    }
    Ok(BindMount {
        host: normalize(host),
        container,
        read_only,
    })
}

/// Parses and cross-checks a mount list: two mounts may not share a target.
pub fn parse_all(specs: &[String]) -> Result<Vec<BindMount>> {
    let mounts = specs.iter().map(|s| parse(s)).collect::<Result<Vec<_>>>()?;
    let mut seen = BTreeSet::new();
    for m in &mounts {
        if !seen.insert(m.container.as_str()) {
            bail!("container path '{}' is mounted more than once", m.container);
        }
    }
    Ok(mounts)
}

fn volume(m: &BindMount) -> String {
    let mode = if m.read_only { ":ro" } else { "" };
    format!("{}:{}{mode}", m.host, m.container)
}

/// Compose override adding `mounts` to the `homeassistant` service. Compose
/// appends override `volumes` to the base list rather than replacing it.
pub fn render_override(mounts: &[BindMount]) -> String {
    let mut out = String::from("# Managed by home-assistant.install; extra bind mounts.\nservices:\n  homeassistant:\n    volumes:\n");
    for m in mounts {
        out.push_str(&format!("      - '{}'\n", volume(m)));
    }
    out
}

/// LXC `mpN:` lines, numbered from 1 (`mp0` is the `/config` volume). Inside
/// the CT the host path is mounted at the container path, so the in-CT
/// override binds `container` to itself.
pub fn render_lxc_mp(mounts: &[BindMount]) -> Vec<String> {
    mounts
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let ro = if m.read_only { ",ro=1" } else { "" };
            format!("mp{}: {},mp={}{ro}", i + 1, m.host, m.container)
        })
        .collect()
}

/// The mounts as seen from inside the LXC: each bind already lives at its
/// container path there.
pub fn in_ct(mounts: &[BindMount]) -> Vec<BindMount> {
    mounts
        .iter()
        .map(|m| BindMount {
            host: m.container.clone(),
            ..m.clone()
        })
        .collect()
}

/// Path of the override that sits beside `compose_file`.
pub fn override_path(compose_file: &str) -> String {
    match Path::new(compose_file).parent() {
        Some(d) if !d.as_os_str().is_empty() => {
            format!("{}/{OVERRIDE_FILE}", d.to_string_lossy())
        }
        _ => OVERRIDE_FILE.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modes_and_normalizes_trailing_slash() {
        assert_eq!(
            parse("/mnt/data/media/:/media:ro").unwrap(),
            BindMount {
                host: "/mnt/data/media".into(),
                container: "/media".into(),
                read_only: true
            }
        );
        assert!(!parse("/mnt/a:/b").unwrap().read_only);
        assert!(!parse("/srv/a:/b:rw").unwrap().read_only);
    }

    #[test]
    fn rejects_malformed_specs() {
        for bad in [
            "/mnt/a",
            "/mnt/a:/b:rx",
            "/mnt/a:/b:ro:x",
            "mnt/a:/b",
            "/mnt/a:b",
            "/mnt/a/../etc:/b",
            "/mnt/a:/b/./c",
            "/mnt/a,ro=1:/b",
            "/mnt/a:/",
            "/mnt/a\n:/b",
            "/mnt/a:/b\n",
            "/mnt/a\t:/b",
            "/mnt/it's:/b",
            "/mnt/a b:/b",
            "/mnt/a:/b'c",
            "/mnt/a:/b#c",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} should be rejected");
        }
    }

    #[test]
    fn host_must_sit_under_an_allowed_root() {
        for bad in [
            "/:/b",
            "/etc:/b",
            "/root/.ssh:/b",
            "/mnt:/b",
            "/mnt/:/b",
            "/mntx/a:/b",
        ] {
            assert!(parse(bad).is_err(), "{bad} should be rejected");
        }
        for ok in ["/mnt/a:/b", "/media/usb:/b", "/srv/x:/b", "/data/y:/b"] {
            assert!(parse(ok).is_ok(), "{ok} should be accepted");
        }
    }

    #[test]
    fn rejects_reserved_targets_and_their_children() {
        for bad in [
            "/mnt/x:/config",
            "/mnt/x:/config/media",
            "/mnt/x:/run/dbus",
            "/mnt/x:/etc/localtime/",
        ] {
            assert!(parse(bad).is_err(), "{bad} should be rejected");
        }
        assert!(parse("/mnt/x:/configs").is_ok());
    }

    #[test]
    fn rejects_duplicate_targets() {
        let specs = vec!["/mnt/a:/media".to_string(), "/mnt/b:/media/".to_string()];
        let err = parse_all(&specs).unwrap_err();
        assert!(err.to_string().contains("more than once"), "{err}");
    }

    #[test]
    fn renders_compose_override() {
        let m = parse_all(&[
            "/mnt/media:/media:ro".to_string(),
            "/srv/b:/backup".to_string(),
        ])
        .unwrap();
        assert_eq!(
            render_override(&m),
            "# Managed by home-assistant.install; extra bind mounts.\nservices:\n  homeassistant:\n    volumes:\n      - '/mnt/media:/media:ro'\n      - '/srv/b:/backup'\n"
        );
    }

    #[test]
    fn renders_lxc_mp_lines_from_one() {
        let m = parse_all(&[
            "/mnt/data/media:/media:ro".to_string(),
            "/srv/b:/backup".to_string(),
        ])
        .unwrap();
        assert_eq!(
            render_lxc_mp(&m),
            [
                "mp1: /mnt/data/media,mp=/media,ro=1",
                "mp2: /srv/b,mp=/backup"
            ]
        );
        assert_eq!(in_ct(&m)[0].host, "/media");
    }

    #[test]
    fn override_sits_beside_compose() {
        assert_eq!(override_path("compose.yml"), "compose.mounts.yml");
        assert_eq!(
            override_path("/opt/homeassistant/compose.yml"),
            "/opt/homeassistant/compose.mounts.yml"
        );
    }
}
