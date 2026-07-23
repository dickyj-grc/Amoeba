//! Local HMAC JWT issuing and verification (standalone/offline mode).

use super::{AuthMode, JwtEngine};
use jsonwebtoken::Validation;

/// Builds a JWT engine configured for local HMAC verification.
pub fn local_engine(secret: impl Into<Vec<u8>>) -> JwtEngine {
    JwtEngine {
        mode: AuthMode::LocalJwt {
            secret: secret.into(),
        },
        validation: Validation::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_a_local_jwt_engine() {
        let engine = local_engine("secret");
        assert!(matches!(engine.mode, AuthMode::LocalJwt { .. }));
    }
}
