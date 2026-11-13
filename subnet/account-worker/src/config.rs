//! The Worker's configuration, from its vars and secrets.
//!
//! Every origin the Worker names derives from `SITE_ORIGIN` and
//! `SECRET_ORIGIN`: redirects, `return_to`, the mail link, the wallet
//! message's domain and URI, the OAuth `redirect_uri` and the CSRF origin
//! check. No host is read from a request and none is compiled in.
//!
//! A config that does not parse leaves the Worker answering
//! `not_configured` (503) on every route that needs the missing piece: no
//! origin, sender or store secret means nothing that needs it runs, and an
//! unset client id or secret leaves that sign-in method absent.

use slop_ninja_auth_core::mail::MailConfig;
use slop_ninja_auth_core::policy::{AuthPolicy, VERSION as AUTH_POLICY_VERSION};
use slop_ninja_auth_core::providers::OAuthProvider;
use slop_ninja_detect_page::origins::Origins;
use slop_ninja_signed_op::{Env as OpEnv, OperatorKey};

use crate::keys::Keys;

/// The var and secret names.
pub mod names {
    pub const SITE_ORIGIN: &str = "SITE_ORIGIN";
    pub const SECRET_ORIGIN: &str = "SECRET_ORIGIN";
    pub const MAIL_SUBJECT_PREFIX: &str = "MAIL_SUBJECT_PREFIX";
    pub const MAIL_FROM: &str = "MAIL_FROM";
    pub const MAIL_AUTO_SUBMITTED: &str = "MAIL_AUTO_SUBMITTED";
    pub const OP_ENV: &str = "OP_ENV";
    pub const DETECT_OP_KEY: &str = "DETECT_OP_KEY";
    pub const AUTH_POLICY: &str = "AUTH_POLICY";
    pub const ACCOUNT_STORE_SECRET: &str = "ACCOUNT_STORE_SECRET";
    pub const GOOGLE_CLIENT_ID: &str = "GOOGLE_CLIENT_ID";
    pub const GITHUB_CLIENT_ID: &str = "GITHUB_CLIENT_ID";
    pub const X_CLIENT_ID: &str = "X_CLIENT_ID";
    pub const GOOGLE_CLIENT_SECRET: &str = "GOOGLE_CLIENT_SECRET";
    pub const GITHUB_CLIENT_SECRET: &str = "GITHUB_CLIENT_SECRET";
    pub const X_CLIENT_SECRET: &str = "X_CLIENT_SECRET";
    /// Dev builds only: the auth stub's origin, where the browser is sent
    /// to authorize and every provider call goes.
    pub const STUB_ORIGIN: &str = "STUB_ORIGIN";
}

/// One OAuth client.
#[derive(Clone)]
pub struct Client {
    pub id: String,
    pub secret: String,
}

impl core::fmt::Debug for Client {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Client").field("id", &self.id).finish_non_exhaustive()
    }
}

/// What a missing or malformed setting leaves unavailable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigError {
    Origins,
    Policy,
    Mail,
    StoreSecret,
    OpEnv,
}

/// The parsed configuration.
#[derive(Clone, Debug)]
pub struct Config {
    pub origins: Origins,
    pub policy: AuthPolicy,
    pub keys: Keys,
    pub mail: Result<MailConfig, ConfigError>,
    pub op_env: OpEnv,
    /// `None`: the op route answers `not_configured`.
    pub op_key: Option<OperatorKey>,
    pub google: Option<Client>,
    pub github: Option<Client>,
    pub x: Option<Client>,
    /// The switch vars (the hard offs), by switch name.
    pub switch_vars: Vec<(&'static str, String)>,
    /// Dev builds: the auth stub's origin.
    pub stub_origin: Option<slop_ninja_detect_page::Origin>,
}

impl Config {
    /// Parses the vars and secrets; `get` answers a var or secret by name.
    pub fn from_vars(get: impl Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
        let var = |k: &str| get(k).map(|v| v.trim().to_owned()).unwrap_or_default();
        let origins = Origins::from_vars(&var(names::SITE_ORIGIN), &var(names::SECRET_ORIGIN))
            .map_err(|_| ConfigError::Origins)?;
        let policy_var = var(names::AUTH_POLICY);
        let policy = if policy_var.is_empty() || policy_var == AUTH_POLICY_VERSION {
            AuthPolicy::v1()
        } else {
            AuthPolicy::from_json(&policy_var).map_err(|_| ConfigError::Policy)?
        };
        let secret = get(names::ACCOUNT_STORE_SECRET).unwrap_or_default();
        let keys = Keys::derive(secret.trim().as_bytes()).ok_or(ConfigError::StoreSecret)?;
        // The prefix is not trimmed: "[staging] " ends in a space.
        let prefix = get(names::MAIL_SUBJECT_PREFIX).unwrap_or_default();
        let mail = MailConfig::new(
            &var(names::MAIL_FROM),
            origins.site.clone(),
            &prefix,
            var(names::MAIL_AUTO_SUBMITTED) == "on",
        )
        .map_err(|_| ConfigError::Mail);
        let op_env = match var(names::OP_ENV).as_str() {
            "production" => OpEnv::Production,
            "staging" => OpEnv::Staging,
            _ => return Err(ConfigError::OpEnv),
        };
        let op_key = OperatorKey::parse(&var(names::DETECT_OP_KEY));
        let client = |id: &str, secret: &str| {
            let (id, secret) = (var(id), var(secret));
            (!id.is_empty() && !secret.is_empty()).then_some(Client { id, secret })
        };
        let switch_vars = slop_ninja_detect_page::switches::SWITCHES
            .iter()
            .filter(|s| s.owner == slop_ninja_detect_page::switches::Owner::Account)
            .map(|s| (s.name, var(s.name)))
            .collect();
        let stub_origin = if cfg!(feature = "dev") {
            slop_ninja_detect_page::Origin::parse(&var(names::STUB_ORIGIN)).ok()
        } else {
            None
        };
        Ok(Config {
            origins,
            policy,
            keys,
            mail,
            op_env,
            op_key,
            google: client(names::GOOGLE_CLIENT_ID, names::GOOGLE_CLIENT_SECRET),
            github: client(names::GITHUB_CLIENT_ID, names::GITHUB_CLIENT_SECRET),
            x: client(names::X_CLIENT_ID, names::X_CLIENT_SECRET),
            switch_vars,
            stub_origin,
        })
    }

    /// The OAuth client of a provider, when its id and secret are both set:
    /// an unset one leaves the method absent (`X_CLIENT_ID` unset keeps X
    /// sign-in off the page entirely).
    pub fn client(&self, p: OAuthProvider) -> Option<&Client> {
        match p {
            OAuthProvider::Google => self.google.as_ref(),
            OAuthProvider::GitHub => self.github.as_ref(),
            OAuthProvider::X => self.x.as_ref(),
        }
    }

    /// The switch var of `name` (empty when unset: the switch's default).
    pub fn switch_var(&self, name: &str) -> &str {
        self.switch_vars.iter().find(|(n, _)| *n == name).map_or("", |(_, v)| v.as_str())
    }

    /// The provider endpoints: the constants, or in a dev build with
    /// `STUB_ORIGIN` set, the same moved under the stub's origin.
    pub fn endpoints(&self, p: OAuthProvider) -> slop_ninja_auth_core::providers::Endpoints {
        #[cfg(feature = "dev")]
        if let Some(stub) = &self.stub_origin {
            return p.spec().endpoints_at(stub);
        }
        p.spec().endpoints()
    }
}
