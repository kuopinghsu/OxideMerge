//! OpenSSH host certificates (`PROTOCOL.certkeys`): a server's key signed by a certificate
//! authority, trusted when the user's known hosts file names that authority for the server
//! (`@cert-authority`). libssh2 checks that the server holds the certificate's key; what the
//! certificate says, and the authority's signature over it, are checked here the way `ssh`
//! checks them.
//!
//! The signature is checked with the algorithms the TLS library already carries: an Ed25519,
//! ECDSA P-256 or P-384, or RSA (SHA-256 or SHA-512) authority. One that signed with SHA-1
//! (`ssh-rsa`), as `ssh` too no longer accepts by default, or with another algorithm, is
//! refused.

use rustls::pki_types::alg_id;
use rustls::pki_types::SignatureVerificationAlgorithm;
use rustls::SignatureScheme;

/// A host certificate's contents.
#[derive(Debug)]
pub(crate) struct HostCert {
    /// The server's own key the certificate is for, as a plain key in its wire form.
    pub key: Vec<u8>,
    /// The authority's public key, in its wire form.
    pub ca: Vec<u8>,
    kind: u32,
    principals: Vec<String>,
    valid_after: u64,
    valid_before: u64,
    critical: Vec<u8>,
    /// What the authority signed: the certificate up to its signature.
    signed_len: usize,
    signature: Vec<u8>,
}

/// The certificate type of a host certificate (a user certificate is 1).
const HOST: u32 = 2;

/// Whether the key `blob` is a certificate rather than a plain key.
pub(crate) fn is_cert(blob: &[u8]) -> bool {
    Reader::new(blob)
        .string()
        .is_some_and(|t| t.ends_with(b"-cert-v01@openssh.com"))
}

/// Reads the SSH wire format: big-endian lengths, and strings prefixed with theirs.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.bytes.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(out)
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_be_bytes(self.take(8)?.try_into().ok()?))
    }

    fn string(&mut self) -> Option<&'a [u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }

    fn done(&self) -> bool {
        self.at == self.bytes.len()
    }
}

fn put_string(out: &mut Vec<u8>, s: &[u8]) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s);
}

/// Reads the certificate `blob`; `Err` says why it can't be.
pub(crate) fn parse(blob: &[u8]) -> Result<HostCert, String> {
    let bad = || "malformed certificate".to_string();
    let mut r = Reader::new(blob);
    let kind = r.string().ok_or_else(bad)?;
    r.string().ok_or_else(bad)?; // nonce
    let mut key = Vec::new();
    match kind {
        b"ssh-ed25519-cert-v01@openssh.com" => {
            put_string(&mut key, b"ssh-ed25519");
            put_string(&mut key, r.string().ok_or_else(bad)?);
        }
        b"ecdsa-sha2-nistp256-cert-v01@openssh.com"
        | b"ecdsa-sha2-nistp384-cert-v01@openssh.com"
        | b"ecdsa-sha2-nistp521-cert-v01@openssh.com" => {
            put_string(&mut key, &kind[..19]);
            put_string(&mut key, r.string().ok_or_else(bad)?);
            put_string(&mut key, r.string().ok_or_else(bad)?);
        }
        b"ssh-rsa-cert-v01@openssh.com" => {
            put_string(&mut key, b"ssh-rsa");
            put_string(&mut key, r.string().ok_or_else(bad)?);
            put_string(&mut key, r.string().ok_or_else(bad)?);
        }
        other => {
            return Err(format!(
                "certificates of {} keys are not supported",
                String::from_utf8_lossy(other)
            ))
        }
    }
    r.u64().ok_or_else(bad)?; // serial
    let cert_kind = r.u32().ok_or_else(bad)?;
    r.string().ok_or_else(bad)?; // key id
    let mut principals = Vec::new();
    let mut list = Reader::new(r.string().ok_or_else(bad)?);
    while !list.done() {
        principals.push(String::from_utf8_lossy(list.string().ok_or_else(bad)?).into_owned());
    }
    let valid_after = r.u64().ok_or_else(bad)?;
    let valid_before = r.u64().ok_or_else(bad)?;
    let critical = r.string().ok_or_else(bad)?.to_vec();
    r.string().ok_or_else(bad)?; // extensions
    r.string().ok_or_else(bad)?; // reserved
    let ca = r.string().ok_or_else(bad)?.to_vec();
    let signed_len = r.at;
    let signature = r.string().ok_or_else(bad)?.to_vec();
    if !r.done() {
        return Err(bad());
    }
    Ok(HostCert {
        key,
        ca,
        kind: cert_kind,
        principals,
        valid_after,
        valid_before,
        critical,
        signed_len,
        signature,
    })
}

/// Checks `cert`, read from `blob`, as a certificate for the server `name` at the time `now`
/// (seconds since 1970): a host certificate, naming the server among its principals (which
/// may be patterns), valid now, with no critical option, and signed by its authority. `Err`
/// says why not.
pub(crate) fn check(cert: &HostCert, blob: &[u8], name: &str, now: u64) -> Result<(), String> {
    if cert.kind != HOST {
        return Err("not a host certificate".to_string());
    }
    let name = name.to_ascii_lowercase();
    if !cert
        .principals
        .iter()
        .any(|p| super::remote::glob_match(name.as_bytes(), p.to_ascii_lowercase().as_bytes()))
    {
        return Err(format!(
            "issued for {}, not {name}",
            match cert.principals.is_empty() {
                true => "no server".to_string(),
                false => cert.principals.join(", "),
            }
        ));
    }
    if now < cert.valid_after {
        return Err("not valid yet".to_string());
    }
    if now >= cert.valid_before {
        return Err("expired".to_string());
    }
    if !cert.critical.is_empty() {
        return Err("has critical options".to_string());
    }
    verify_signature(&cert.ca, &blob[..cert.signed_len], &cert.signature)
}

/// Checks `signature` (its SSH wire form) of `message` by the key `ca` (its wire form).
fn verify_signature(ca: &[u8], message: &[u8], signature: &[u8]) -> Result<(), String> {
    let bad = || "malformed signature".to_string();
    let mut s = Reader::new(signature);
    let alg = s.string().ok_or_else(bad)?;
    let sig = s.string().ok_or_else(bad)?;
    let mut k = Reader::new(ca);
    let ca_kind = k.string().ok_or_else(bad)?;
    let (scheme, key_alg, public_key, sig) = match (alg, ca_kind) {
        (b"ssh-ed25519", b"ssh-ed25519") => (
            SignatureScheme::ED25519,
            alg_id::ED25519,
            k.string().ok_or_else(bad)?.to_vec(),
            sig.to_vec(),
        ),
        (b"ecdsa-sha2-nistp256", b"ecdsa-sha2-nistp256")
        | (b"ecdsa-sha2-nistp384", b"ecdsa-sha2-nistp384") => {
            k.string().ok_or_else(bad)?; // curve name
            let point = k.string().ok_or_else(bad)?.to_vec();
            let mut rs = Reader::new(sig);
            let (r, s) = (rs.string().ok_or_else(bad)?, rs.string().ok_or_else(bad)?);
            let der = der_sequence(&[&der_integer(r), &der_integer(s)]);
            if alg == b"ecdsa-sha2-nistp256" {
                (
                    SignatureScheme::ECDSA_NISTP256_SHA256,
                    alg_id::ECDSA_P256,
                    point,
                    der,
                )
            } else {
                (
                    SignatureScheme::ECDSA_NISTP384_SHA384,
                    alg_id::ECDSA_P384,
                    point,
                    der,
                )
            }
        }
        (b"rsa-sha2-256" | b"rsa-sha2-512", b"ssh-rsa") => {
            let e = k.string().ok_or_else(bad)?;
            let n = k.string().ok_or_else(bad)?;
            let scheme = if alg == b"rsa-sha2-256" {
                SignatureScheme::RSA_PKCS1_SHA256
            } else {
                SignatureScheme::RSA_PKCS1_SHA512
            };
            let key = der_sequence(&[&der_integer(n), &der_integer(e)]);
            (scheme, alg_id::RSA_ENCRYPTION, key, sig.to_vec())
        }
        _ => {
            return Err(format!(
                "signed with {}, which is not accepted",
                String::from_utf8_lossy(alg)
            ))
        }
    };
    let provider = rustls::crypto::ring::default_provider();
    let algorithm: Option<&dyn SignatureVerificationAlgorithm> = provider
        .signature_verification_algorithms
        .mapping
        .iter()
        .filter(|(s, _)| *s == scheme)
        .flat_map(|(_, algs)| algs.iter().copied())
        .find(|a| a.public_key_alg_id() == key_alg);
    let algorithm = algorithm.ok_or_else(|| "unsupported signature".to_string())?;
    algorithm
        .verify_signature(&public_key, message, &sig)
        .map_err(|_| "its authority's signature does not match".to_string())
}

/// The DER length of `n` bytes of contents.
fn der_len(n: usize, out: &mut Vec<u8>) {
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let bytes: Vec<u8> = n
            .to_be_bytes()
            .into_iter()
            .skip_while(|&b| b == 0)
            .collect();
        out.push(0x80 | bytes.len() as u8);
        out.extend(bytes);
    }
}

/// A DER `INTEGER` of the non-negative big-endian number `n` (an SSH `mpint`).
fn der_integer(n: &[u8]) -> Vec<u8> {
    let mut digits: &[u8] = n;
    while digits.len() > 1 && digits[0] == 0 && digits[1] & 0x80 == 0 {
        digits = &digits[1..];
    }
    let pad = digits.first().is_none_or(|&b| b & 0x80 != 0);
    let mut out = vec![0x02];
    der_len(digits.len() + pad as usize, &mut out);
    if pad {
        out.push(0);
    }
    out.extend_from_slice(digits);
    out
}

/// A DER `SEQUENCE` of `parts`, each already encoded.
fn der_sequence(parts: &[&[u8]]) -> Vec<u8> {
    let len = parts.iter().map(|p| p.len()).sum();
    let mut out = vec![0x30];
    der_len(len, &mut out);
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Vec<u8> {
        let path = format!(
            "{}/tests/fixtures/ssh-certs/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        let text = std::fs::read_to_string(path).unwrap();
        let b64 = text.split_whitespace().nth(1).unwrap();
        super::super::remote::decode_base64(b64).unwrap()
    }

    /// 2026-11-08, between the fixtures' validity periods.
    const NOW: u64 = 1_794_000_000;

    #[test]
    fn certificates_signed_by_each_kind_of_authority_are_accepted() {
        let host = fixture("host.pub");
        for (cert, ca) in [
            ("h_ed-cert.pub", "ca_ed.pub"),
            ("h_ec-cert.pub", "ca_ec.pub"),
            ("h_rsa-cert.pub", "ca_rsa.pub"),
        ] {
            let blob = fixture(cert);
            assert!(is_cert(&blob));
            let c = parse(&blob).unwrap();
            assert_eq!(c.key, host, "{cert}");
            assert_eq!(c.ca, fixture(ca), "{cert}");
            check(&c, &blob, "host.example", NOW).unwrap();
            check(&c, &blob, "HOST.example", NOW).unwrap();
            check(&c, &blob, "db.wild.example", NOW).unwrap();
            assert!(check(&c, &blob, "other.example", NOW).is_err(), "{cert}");
        }
        assert!(!is_cert(&host));
        let blob = fixture("h384-cert.pub");
        let c = parse(&blob).unwrap();
        assert!(c.key.starts_with(b"\0\0\0\x13ecdsa-sha2-nistp384"));
        check(&c, &blob, "host.example", NOW).unwrap();
    }

    #[test]
    fn certificates_out_of_date_for_users_naming_nobody_or_altered_are_refused() {
        let refused = |name: &str| {
            let blob = fixture(name);
            check(&parse(&blob).unwrap(), &blob, "host.example", NOW).unwrap_err()
        };
        assert_eq!(refused("expired-cert.pub"), "expired");
        assert_eq!(refused("future-cert.pub"), "not valid yet");
        assert_eq!(refused("user-cert.pub"), "not a host certificate");
        assert!(refused("noprinc-cert.pub").contains("no server"));

        let mut blob = fixture("h_ed-cert.pub");
        let id = blob.windows(7).position(|w| w == b"host-ed").unwrap();
        blob[id] ^= 1;
        let altered = parse(&blob).unwrap();
        assert!(check(&altered, &blob, "host.example", NOW)
            .unwrap_err()
            .contains("signature"));
        assert!(parse(&blob[..blob.len() - 1]).is_err());
        assert!(parse(b"\0\0\0\x05short").is_err());
    }

    #[test]
    fn der_integers_are_minimal_and_never_negative() {
        assert_eq!(der_integer(&[0x7f]), [2, 1, 0x7f]);
        assert_eq!(der_integer(&[0x80]), [2, 2, 0, 0x80]);
        assert_eq!(der_integer(&[0, 0, 0x05]), [2, 1, 5]);
        assert_eq!(der_integer(&[0, 0x80]), [2, 2, 0, 0x80]);
        let long = der_sequence(&[&[0u8; 200]]);
        assert_eq!(&long[..3], &[0x30, 0x81, 200]);
    }
}
