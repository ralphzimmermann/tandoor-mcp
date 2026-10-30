//! Authentication handling for Tandoor API.
//!
//! Tandoor 2.7 removed its username/password token endpoint. Callers must
//! provide an API token created under Tandoor's Settings → API page.

/// # Tandoor Authentication Handler
///
/// Stores the OAuth2 Bearer token used for Tandoor API access.
///
/// ## Token Format
///
/// Tandoor uses OAuth2 access tokens with the format `tda_xxxxxxxx_xxxx_xxxx_xxxx_xxxxxxxxxxxx`.
/// These tokens are used with `Bearer` authentication in API requests.
#[derive(Clone, Default)]
pub struct TandoorAuth {
    /// Configured API token.
    token: Option<String>,
}

impl TandoorAuth {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    pub fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }

    pub fn set_token(&mut self, token: String) {
        self.token = Some(token);
    }
}
