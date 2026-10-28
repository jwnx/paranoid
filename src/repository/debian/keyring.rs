//! OpenPGP keyrings, and the clearsigned `InRelease` files and detached
//! signatures they verify.
//!
//! rpgp does the cryptography. What it leaves to us is policy, and we
//! follow what `gpgv` enforces for apt:
//!
//! - a key counts only through a valid self-signature or subkey binding,
//!   which also says whether it may sign and when it expires;
//! - revoked and expired keys don't count;
//! - SHA-1 and weaker digests don't count;
//! - a signature from a key in the keyring that doesn't verify is fatal,
//!   since that is what tampering looks like;
//! - signatures from keys outside the keyring are ignored, and at least one
//!   good signature is required.

use std::fmt;
use std::path::Path;

use chrono::{DateTime, Utc};
use pgp::composed::{
    CleartextSignedMessage, Deserializable, DetachedSignature, PublicOrSecret, SignedPublicKey,
};
use pgp::crypto::hash::HashAlgorithm;
use pgp::packet::{PublicKey, PublicSubkey, Signature, SignatureType};
use pgp::types::{KeyDetails, Tag, Timestamp};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// An OpenPGP fingerprint, as upper-case hex.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Fingerprint(String);

impl Fingerprint {
    fn of(key: &impl KeyDetails) -> Self {
        Self(format!("{:X}", key.fingerprint()))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Error loading a keyring.
#[derive(Debug, Error)]
pub enum KeyringError {
    #[error("could not read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("invalid OpenPGP key data: {0}")]
    Parse(#[source] pgp::errors::Error),

    #[error("no OpenPGP public keys found")]
    Empty,

    #[error("key {0} has no valid self-signature")]
    NoSelfSignature(Fingerprint),
}

/// Error verifying a signed message.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum VerifyError {
    #[error("not a valid clearsigned message: {0}")]
    Malformed(String),

    #[error("not a valid OpenPGP signature: {0}")]
    MalformedSignature(String),

    #[error("BAD signature from key {0}: the content was modified or corrupted")]
    BadSignature(Fingerprint),

    #[error("no valid signature from a trusted key ({})", .0.join("; "))]
    NoValidSignature(Vec<String>),
}

/// A message whose signature checked out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The signed text, with `\n` line endings. Nothing outside the
    /// signature is ever returned.
    pub text: String,
    /// Primary fingerprints of the keys with a good signature, sorted.
    pub signers: Vec<Fingerprint>,
}

/// When a key has to have been valid for its signature to count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTime {
    /// At the time we check: a repository's Release is only as good as the
    /// keys that sign it are now.
    Now,
    /// When it made the signature, as gpg judges a release artifact: a key
    /// that expired after signing still vouches for what it signed. A
    /// revoked key still doesn't count.
    Signing,
}

/// Keys trusted to sign a repository.
#[derive(Debug, Clone)]
pub struct Keyring {
    signers: Vec<Signer>,
}

/// A key that may make signatures: a primary key or a subkey, with the
/// policy facts its self-signatures establish.
#[derive(Debug, Clone)]
struct Signer {
    key: SignerKey,
    /// The primary key's fingerprint, which is what people recognize.
    primary: Fingerprint,
    can_sign: bool,
    revoked: bool,
    /// Seconds since the epoch, `None` if the key never expires.
    expires: Option<u64>,
}

#[derive(Debug, Clone)]
enum SignerKey {
    Primary(PublicKey),
    Subkey(PublicSubkey),
}

impl SignerKey {
    fn fingerprint(&self) -> pgp::types::Fingerprint {
        match self {
            SignerKey::Primary(k) => k.fingerprint(),
            SignerKey::Subkey(k) => k.fingerprint(),
        }
    }

    fn key_id(&self) -> pgp::types::KeyId {
        match self {
            SignerKey::Primary(k) => k.legacy_key_id(),
            SignerKey::Subkey(k) => k.legacy_key_id(),
        }
    }

    fn verify(&self, sig: &Signature, data: &[u8]) -> bool {
        match self {
            SignerKey::Primary(k) => sig.verify(k, data).is_ok(),
            SignerKey::Subkey(k) => sig.verify(k, data).is_ok(),
        }
    }
}

impl Keyring {
    /// Load keys from a file, ASCII-armored or binary.
    pub fn load(path: &Path) -> Result<Self, KeyringError> {
        let data = std::fs::read(path).map_err(|source| KeyringError::Io {
            path: path.display().to_string(),
            source,
        })?;
        Self::from_bytes(&data)
    }

    /// Parse keys, ASCII-armored or binary, possibly several concatenated.
    pub fn from_bytes(data: &[u8]) -> Result<Self, KeyringError> {
        let mut signers = Vec::new();
        for chunk in armor_blocks(data) {
            let (keys, _) = PublicOrSecret::from_reader_many(chunk).map_err(KeyringError::Parse)?;
            for key in keys {
                let key = match key.map_err(KeyringError::Parse)? {
                    PublicOrSecret::Public(k) => k,
                    PublicOrSecret::Secret(k) => k.to_public_key(),
                };
                signers.extend(signers_of(&key)?);
            }
        }
        if signers.is_empty() {
            return Err(KeyringError::Empty);
        }
        Ok(Self { signers })
    }

    /// Primary fingerprints of every key in the keyring, sorted.
    pub fn fingerprints(&self) -> Vec<Fingerprint> {
        let mut fps: Vec<_> = self.signers.iter().map(|s| s.primary.clone()).collect();
        fps.sort();
        fps.dedup();
        fps
    }

    /// Verify a clearsigned message, as of `now`.
    pub fn verify_cleartext(
        &self,
        input: &str,
        now: DateTime<Utc>,
    ) -> Result<Verified, VerifyError> {
        self.verify_cleartext_with(input, now, KeyTime::Now)
    }

    /// Verify a clearsigned message, as of `now`, with keys judged at `keys`.
    pub fn verify_cleartext_with(
        &self,
        input: &str,
        now: DateTime<Utc>,
        keys: KeyTime,
    ) -> Result<Verified, VerifyError> {
        // rpgp refuses anything before the armor header or after the
        // signature block, so text outside the signature can't sneak in.
        let (msg, _) = CleartextSignedMessage::from_string(input)
            .map_err(|e| VerifyError::Malformed(e.to_string()))?;
        let signed = msg.signed_text();
        let now = now.timestamp().max(0) as u64;

        let signers = self.judge(msg.signatures(), signed.as_bytes(), now, keys)?;
        Ok(Verified {
            text: signed.replace("\r\n", "\n"),
            signers,
        })
    }

    /// Verify a detached signature over `data`, ASCII-armored or binary, as
    /// of `now`, with keys judged at `keys`. Returns the primary
    /// fingerprints of the keys with a good signature, sorted.
    pub fn verify_detached(
        &self,
        signature: &[u8],
        data: &[u8],
        now: DateTime<Utc>,
        keys: KeyTime,
    ) -> Result<Vec<Fingerprint>, VerifyError> {
        let malformed = |e: pgp::errors::Error| VerifyError::MalformedSignature(e.to_string());
        let (sigs, _) = DetachedSignature::from_reader_many(signature).map_err(malformed)?;
        let sigs: Vec<Signature> = sigs
            .map(|s| s.map(|s| s.signature))
            .collect::<Result<_, _>>()
            .map_err(malformed)?;
        let now = now.timestamp().max(0) as u64;
        self.judge(&sigs, data, now, keys)
    }

    /// The signers of every signature that counts, sorted, when at least
    /// one does.
    fn judge<'s>(
        &self,
        sigs: impl IntoIterator<Item = &'s Signature>,
        data: &[u8],
        now: u64,
        keys: KeyTime,
    ) -> Result<Vec<Fingerprint>, VerifyError> {
        let mut signers = Vec::new();
        let mut rejected = Vec::new();
        for sig in sigs {
            match self.check(sig, data, now, keys)? {
                Ok(fp) => signers.push(fp),
                Err(reason) => rejected.push(reason),
            }
        }

        if signers.is_empty() {
            if rejected.is_empty() {
                rejected.push("there are no signatures".to_string());
            }
            return Err(VerifyError::NoValidSignature(rejected));
        }
        signers.sort();
        signers.dedup();
        Ok(signers)
    }

    /// Check one signature. The outer error is fatal; the inner one says
    /// why this signature doesn't count.
    fn check(
        &self,
        sig: &Signature,
        data: &[u8],
        now: u64,
        keys: KeyTime,
    ) -> Result<Result<Fingerprint, String>, VerifyError> {
        // A signature naming its issuer only by key ID can match more than
        // one key; it's BAD only if none of them verify it.
        let mut candidates = self.signers.iter().filter(|s| issued_by(sig, s)).peekable();
        let Some(first) = candidates.peek() else {
            let issuer = sig
                .issuer_fingerprint()
                .first()
                .map(|fp| format!("{fp:X}"))
                .or_else(|| sig.issuer_key_id().first().map(|id| format!("{id:?}")))
                .unwrap_or_else(|| "an unnamed key".to_string());
            return Ok(Err(format!(
                "signed by {issuer}, which is not in the keyring"
            )));
        };
        // A weak digest doesn't count whether or not it verifies, and rpgp
        // refuses to verify some, so check it before calling it BAD.
        if is_weak(sig.hash_alg()) {
            let fp = &first.primary;
            return Ok(Err(format!("signature by {fp} uses a weak digest")));
        }
        let bad = VerifyError::BadSignature(first.primary.clone());
        let Some(signer) = candidates.find(|s| s.key.verify(sig, data)) else {
            return Err(bad);
        };
        let fp = &signer.primary;

        let created = sig.created().map_or(0, |t| u64::from(t.as_secs()));
        let key_time = match keys {
            KeyTime::Now => now,
            KeyTime::Signing => created,
        };
        let reason = if !matches!(sig.typ(), Some(SignatureType::Text | SignatureType::Binary)) {
            Some("is not a document signature")
        } else if signer.revoked {
            Some("is from a revoked key")
        } else if !signer.can_sign {
            Some("is from a key not allowed to sign")
        } else if signer.expires.is_some_and(|t| t <= key_time) {
            Some("is from an expired key")
        } else if expiry(created, sig.signature_expiration_time()).is_some_and(|t| t <= now) {
            Some("has expired")
        } else {
            None
        };
        Ok(match reason {
            Some(reason) => Err(format!("signature by {fp} {reason}")),
            None => Ok(fp.clone()),
        })
    }
}

/// Split armored data into one chunk per armor block, since rpgp reads only
/// the first. Binary keyrings hold any number of keys and pass through.
fn armor_blocks(data: &[u8]) -> Vec<&[u8]> {
    const BEGIN: &[u8] = b"-----BEGIN PGP ";
    let starts: Vec<usize> = (0..data.len())
        .filter(|&i| data[i..].starts_with(BEGIN) && (i == 0 || data[i - 1] == b'\n'))
        .collect();
    if starts.is_empty() {
        return vec![data];
    }
    let ends = starts.iter().skip(1).copied().chain([data.len()]);
    starts.iter().zip(ends).map(|(&a, b)| &data[a..b]).collect()
}

fn is_weak(hash: Option<HashAlgorithm>) -> bool {
    !matches!(
        hash,
        Some(
            HashAlgorithm::Sha224
                | HashAlgorithm::Sha256
                | HashAlgorithm::Sha384
                | HashAlgorithm::Sha512
                | HashAlgorithm::Sha3_256
                | HashAlgorithm::Sha3_512
        )
    )
}

fn issued_by(sig: &Signature, signer: &Signer) -> bool {
    let fps = sig.issuer_fingerprint();
    if !fps.is_empty() {
        return fps.iter().any(|fp| **fp == signer.key.fingerprint());
    }
    let id = signer.key.key_id();
    sig.issuer_key_id().iter().any(|i| **i == id)
}

/// When something created at `created` expires after `lifetime`.
/// A zero or missing lifetime means never.
fn expiry(created: u64, lifetime: Option<pgp::types::Duration>) -> Option<u64> {
    lifetime
        .map(|d| u64::from(d.as_secs()))
        .filter(|&secs| secs > 0)
        .map(|secs| created + secs)
}

fn created(sig: &Signature) -> Timestamp {
    sig.created().unwrap_or(Timestamp::from_secs(0))
}

/// The primary key and each subkey of `key`, as signers.
fn signers_of(key: &SignedPublicKey) -> Result<Vec<Signer>, KeyringError> {
    let primary = &key.primary_key;
    let primary_fp = Fingerprint::of(primary);

    // The newest valid self-signature carries the primary key's flags and
    // expiry. Third-party certifications don't verify against the primary
    // key and are skipped.
    let users = key.details.users.iter().flat_map(|u| {
        u.signatures
            .iter()
            .filter(|s| s.verify_certification(primary, Tag::UserId, &u.id).is_ok())
    });
    let direct = key
        .details
        .direct_signatures
        .iter()
        .filter(|s| s.verify_key(primary).is_ok());
    let self_sig = users
        .chain(direct)
        .max_by_key(|s| created(s))
        .ok_or_else(|| KeyringError::NoSelfSignature(primary_fp.clone()))?;

    let revoked = key
        .details
        .revocation_signatures
        .iter()
        .any(|s| s.verify_key(primary).is_ok());
    let primary_created = u64::from(primary.created_at().as_secs());
    let primary_expires = expiry(primary_created, self_sig.key_expiration_time());
    let flags = self_sig.key_flags();
    // Old keys without a key flags subpacket may sign with the primary.
    let primary_can_sign = flags.sign() || flags == Default::default();

    let mut signers = vec![Signer {
        key: SignerKey::Primary(primary.clone()),
        primary: primary_fp.clone(),
        can_sign: primary_can_sign,
        revoked,
        expires: primary_expires,
    }];

    for sub in &key.public_subkeys {
        let valid = |s: &&Signature| s.verify_subkey_binding(primary, &sub.key).is_ok();
        let binding = sub
            .signatures
            .iter()
            .filter(|s| s.typ() == Some(SignatureType::SubkeyBinding))
            .filter(valid)
            .max_by_key(|s| created(s));
        let Some(binding) = binding else {
            // An unbound subkey isn't part of the key; ignore it.
            continue;
        };
        // A signing subkey must also sign the primary back, or anyone could
        // claim someone else's signing key as their subkey.
        let can_sign = binding.key_flags().sign()
            && binding
                .embedded_signature()
                .is_some_and(|back| back.verify_primary_key_binding(&sub.key, primary).is_ok());
        let sub_revoked = sub
            .signatures
            .iter()
            .filter(|s| s.typ() == Some(SignatureType::SubkeyRevocation))
            .any(|s| valid(&s));
        let sub_created = u64::from(sub.key.created_at().as_secs());
        let sub_expires = expiry(sub_created, binding.key_expiration_time());
        signers.push(Signer {
            key: SignerKey::Subkey(sub.key.clone()),
            primary: primary_fp.clone(),
            can_sign,
            revoked: revoked || sub_revoked,
            // A subkey can't outlive its primary.
            expires: match (sub_expires, primary_expires) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
        });
    }

    Ok(signers)
}

#[cfg(test)]
mod tests {
    use super::*;

    const BOOKWORM_INRELEASE: &str = include_str!("testdata/bookworm.InRelease");
    const ARCHIVE_12: &[u8] = include_bytes!("testdata/debian-archive-bookworm-automatic.asc");
    const RELEASE_12: &[u8] = include_bytes!("testdata/debian-archive-bookworm-stable.asc");
    const TRIXIE_13: &[u8] = include_bytes!("testdata/debian-archive-trixie-automatic.asc");

    /// When the test data was captured; its keys are all valid then.
    fn then() -> DateTime<Utc> {
        "2026-10-15T00:00:00Z".parse().unwrap()
    }

    fn keyring(parts: &[&[u8]]) -> Keyring {
        Keyring::from_bytes(&parts.concat()).unwrap()
    }

    #[test]
    fn loads_debian_keys() {
        let fps = keyring(&[ARCHIVE_12, RELEASE_12]).fingerprints();
        let fps: Vec<_> = fps.iter().map(ToString::to_string).collect();
        assert_eq!(
            fps,
            [
                "4D64FEC119C2029067D6E791F8D2585B8783D481",
                "B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8",
            ]
        );
    }

    #[test]
    fn verifies_a_real_inrelease() {
        // Archive key 12 signs with an RSA subkey, the release key with an
        // Ed25519 primary key; both must work on their own.
        for (key, fp) in [
            (ARCHIVE_12, "B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8"),
            (RELEASE_12, "4D64FEC119C2029067D6E791F8D2585B8783D481"),
            (TRIXIE_13, "04B54C3CDCA79751B16BC6B5225629DF75B188BD"),
        ] {
            let v = keyring(&[key])
                .verify_cleartext(BOOKWORM_INRELEASE, then())
                .unwrap();
            assert_eq!(v.signers.len(), 1);
            assert_eq!(v.signers[0].to_string(), fp);
            assert!(v.text.starts_with("Origin: Debian\n"));
            assert!(!v.text.contains("PGP"));
        }

        let all = keyring(&[ARCHIVE_12, RELEASE_12, TRIXIE_13]);
        let v = all.verify_cleartext(BOOKWORM_INRELEASE, then()).unwrap();
        assert_eq!(v.signers.len(), 3);
    }

    #[test]
    fn rejects_a_modified_inrelease() {
        let tampered = BOOKWORM_INRELEASE.replacen("Codename: bookworm", "Codename: bookwurm", 1);
        assert_ne!(tampered, BOOKWORM_INRELEASE);
        let err = keyring(&[ARCHIVE_12])
            .verify_cleartext(&tampered, then())
            .unwrap_err();
        assert!(matches!(err, VerifyError::BadSignature(_)), "{err}");
    }

    #[test]
    fn rejects_text_outside_the_signature() {
        let ring = keyring(&[ARCHIVE_12]);
        let before = format!("Origin: Evil\n\n{BOOKWORM_INRELEASE}");
        let after = format!("{BOOKWORM_INRELEASE}\nPackage: evil\n");
        for input in [before, after] {
            let err = ring.verify_cleartext(&input, then()).unwrap_err();
            assert!(matches!(err, VerifyError::Malformed(_)), "{err}");
        }
    }

    #[test]
    fn ignores_unknown_signers_but_needs_one_good_one() {
        // A keyring of unrelated keys: every signature is from a stranger.
        let bookworm_security =
            include_bytes!("testdata/debian-archive-bookworm-security-automatic.asc");
        let err = keyring(&[bookworm_security])
            .verify_cleartext(BOOKWORM_INRELEASE, then())
            .unwrap_err();
        let VerifyError::NoValidSignature(reasons) = err else {
            panic!("{err}");
        };
        assert_eq!(reasons.len(), 3);
        assert!(reasons.iter().all(|r| r.contains("not in the keyring")));
    }

    #[test]
    fn rejects_expired_keys() {
        // Archive key 12 expires in 2031.
        let later: DateTime<Utc> = "2032-01-01T00:00:00Z".parse().unwrap();
        let err = keyring(&[ARCHIVE_12])
            .verify_cleartext(BOOKWORM_INRELEASE, later)
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "no valid signature from a trusted key (signature by \
             B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8 is from an expired key; \
             signed by B8E5F13176D2A7A75220028078DBA3BC47EF2265, which is not in the keyring; \
             signed by 4D64FEC119C2029067D6E791F8D2585B8783D481, which is not in the keyring)"
        );
    }

    #[test]
    fn rejects_garbage_keyrings() {
        assert!(Keyring::from_bytes(b"").is_err());
        assert!(Keyring::from_bytes(b"not a key").is_err());
    }

    mod detached {
        use super::*;

        const TOOL: &[u8] = include_bytes!("testdata/files/tool");
        const TOOL_SIG: &[u8] = include_bytes!("testdata/files/tool.asc");
        const SUMS: &[u8] = include_bytes!("testdata/files/SHA256SUMS");
        const SUMS_SIG: &[u8] = include_bytes!("testdata/files/SHA256SUMS.sig");
        const SUMS_CLEARSIGNED: &str = include_str!("testdata/files/SHA256SUMS.asc");
        const REVOKED_SIG: &[u8] = include_bytes!("testdata/files/tool.revoked.asc");
        const RELEASE: &[u8] = include_bytes!("testdata/files/release.asc");
        const REVOKED: &[u8] = include_bytes!("testdata/files/revoked.asc");
        const STRANGER: &[u8] = include_bytes!("testdata/files/stranger.asc");
        const RELEASE_FP: &str = "7B6CFBF9384A130F6053229331B1E3B66D0E9BF6";

        /// After the release key expired: it signed in 2020 and expired at
        /// the start of 2021.
        fn now() -> DateTime<Utc> {
            "2026-10-28T00:00:00Z".parse().unwrap()
        }

        fn fps(fps: Vec<Fingerprint>) -> Vec<String> {
            fps.iter().map(ToString::to_string).collect()
        }

        #[test]
        fn verifies_armored_and_binary_signatures() {
            let ring = keyring(&[RELEASE]);
            let signed = ring.verify_detached(TOOL_SIG, TOOL, now(), KeyTime::Signing);
            assert_eq!(fps(signed.unwrap()), [RELEASE_FP]);
            let signed = ring.verify_detached(SUMS_SIG, SUMS, now(), KeyTime::Signing);
            assert_eq!(fps(signed.unwrap()), [RELEASE_FP]);
            let v = ring
                .verify_cleartext_with(SUMS_CLEARSIGNED, now(), KeyTime::Signing)
                .unwrap();
            // The line break before the signature belongs to it.
            assert_eq!(v.text.as_bytes(), SUMS.trim_ascii_end());
        }

        #[test]
        fn a_key_that_expired_after_signing_counts_only_when_judged_at_signing() {
            let ring = keyring(&[RELEASE]);
            let err = ring
                .verify_detached(TOOL_SIG, TOOL, now(), KeyTime::Now)
                .unwrap_err();
            assert!(err.to_string().contains("expired key"), "{err}");
            let err = ring.verify_cleartext(SUMS_CLEARSIGNED, now()).unwrap_err();
            assert!(err.to_string().contains("expired key"), "{err}");
        }

        #[test]
        fn rejects_modified_data() {
            let err = keyring(&[RELEASE])
                .verify_detached(TOOL_SIG, b"a tool, changed\n", now(), KeyTime::Signing)
                .unwrap_err();
            assert!(matches!(err, VerifyError::BadSignature(_)), "{err}");
        }

        #[test]
        fn rejects_revoked_keys_and_strangers() {
            let err = keyring(&[REVOKED])
                .verify_detached(REVOKED_SIG, TOOL, now(), KeyTime::Signing)
                .unwrap_err();
            assert!(err.to_string().contains("revoked key"), "{err}");

            let err = keyring(&[STRANGER])
                .verify_detached(TOOL_SIG, TOOL, now(), KeyTime::Signing)
                .unwrap_err();
            assert!(err.to_string().contains("not in the keyring"), "{err}");
        }

        #[test]
        fn rejects_what_is_not_a_signature() {
            let err = keyring(&[RELEASE])
                .verify_detached(b"not a signature", TOOL, now(), KeyTime::Signing)
                .unwrap_err();
            assert!(matches!(err, VerifyError::MalformedSignature(_)), "{err}");
            let err = keyring(&[RELEASE])
                .verify_detached(RELEASE, TOOL, now(), KeyTime::Signing)
                .unwrap_err();
            assert!(matches!(err, VerifyError::MalformedSignature(_)), "{err}");
        }
    }
}
