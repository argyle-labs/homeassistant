//! Admin gate for mutating verbs.
//!
//! A dry run is gated exactly like an execute: the plan shows what would
//! change, so producing it needs the same authority as making the change.
#![allow(clippy::disallowed_types)]

use plugin_toolkit::contract::CallerIdentity;
use plugin_toolkit::prelude::*;

/// Fails unless the caller has the `admin` role. Call before any planning.
pub(crate) fn require_admin(ctx: &ToolCtx) -> Result<()> {
    check(ctx.caller().as_ref())
}

fn check(caller: Option<&CallerIdentity>) -> Result<()> {
    match caller {
        Some(c) if c.role == "admin" => Ok(()),
        Some(c) => bail!(
            "admin role required; '{}' has role '{}'",
            c.username,
            c.role
        ),
        None => bail!("admin role required; the call carries no caller identity"),
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    /// A tool context whose caller has `role`, or no caller at all.
    pub(crate) fn ctx(role: Option<&str>) -> ToolCtx {
        let base = plugin_toolkit::tool_manifest::minimal_ctx();
        match role {
            Some(r) => base.with_auth(CallerIdentity {
                user_id: "u-test".into(),
                username: "tester".into(),
                role: r.into(),
                can_mutate: true,
            }),
            None => base,
        }
    }

    /// Asserts `err` is the admin refusal.
    pub(crate) fn assert_refused(err: &plugin_toolkit::anyhow::Error) {
        assert!(err.to_string().contains("admin role required"), "{err}");
    }
}

#[cfg(test)]
mod tests {
    use super::testing::ctx;
    use super::*;

    #[test]
    fn only_admin_passes() {
        assert!(require_admin(&ctx(Some("admin"))).is_ok());
        for refused in [
            ctx(Some("user")),
            ctx(Some("viewer")),
            ctx(Some("")),
            ctx(None),
        ] {
            let err = require_admin(&refused).unwrap_err();
            assert!(err.to_string().contains("admin role required"), "{err}");
        }
    }
}
