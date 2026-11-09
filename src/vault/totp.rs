//! Time-based one-time codes (RFC 6238) from an `otpauth://` URI or a
//! Base32 secret.

use totp_rs::{Builder, Secret, Totp};

/// The periods and lengths accepted: what authenticator apps use.
const PERIODS: std::ops::RangeInclusive<u64> = 1..=300;
const DIGITS: std::ops::RangeInclusive<u8> = 6..=8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TotpCode {
    pub code: String,
    /// Seconds the code stays valid.
    pub remaining: u64,
    pub period: u64,
}

fn parse(input: &str) -> Result<Totp, String> {
    let input = input.trim();
    if input.is_empty() {
        return Err("验证码密钥为空".into());
    }
    if input.to_ascii_lowercase().starts_with("otpauth://") {
        let totp = Totp::from_url_unchecked(input).map_err(|_| "无效的 otpauth 链接".to_owned())?;
        // Unchecked parsing allows short secrets (as sites issue them) but
        // also a period of 0 or absurd lengths, which would divide by zero
        // or overflow when computing a code.
        if !PERIODS.contains(&totp.step()) || !DIGITS.contains(&totp.digits()) {
            return Err("不支持的验证码周期或位数".into());
        }
        return Ok(totp);
    }
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect::<String>()
        .to_ascii_uppercase();
    let secret = Secret::try_from_base32(cleaned.trim_end_matches('='))
        .map_err(|_| "无效的 Base32 密钥".to_owned())?;
    if secret.is_empty() {
        return Err("无效的 Base32 密钥".into());
    }
    Ok(Builder::new().with_secret(secret).build_noncompliant())
}

/// Whether `input` is a usable TOTP secret.
pub fn validate(input: &str) -> Result<(), String> {
    parse(input).map(|_| ())
}

/// The code at `unix_secs`.
pub fn code_at(input: &str, unix_secs: u64) -> Result<TotpCode, String> {
    let totp = parse(input)?;
    let period = totp.step();
    Ok(TotpCode {
        code: totp.generate(unix_secs).to_string(),
        remaining: period - unix_secs % period,
        period,
    })
}

/// The current code.
pub fn current(input: &str) -> Result<TotpCode, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    code_at(input, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 6238 seeds: "12345678901234567890" (SHA1), 32 bytes (SHA256), 64 bytes (SHA512).
    const SHA1: &str = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ&algorithm=SHA1&digits=8&period=30";
    const SHA256: &str = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZA&algorithm=SHA256&digits=8&period=30";
    const SHA512: &str = "otpauth://totp/x?secret=GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQGEZDGNA&algorithm=SHA512&digits=8&period=30";

    #[test]
    fn rfc6238_vectors() {
        for (time, sha1, sha256, sha512) in [
            (59, "94287082", "46119246", "90693936"),
            (1_111_111_109, "07081804", "68084774", "25091201"),
            (2_000_000_000, "69279037", "90698825", "38618901"),
        ] {
            assert_eq!(code_at(SHA1, time).unwrap().code, sha1);
            assert_eq!(code_at(SHA256, time).unwrap().code, sha256);
            assert_eq!(code_at(SHA512, time).unwrap().code, sha512);
        }
    }

    #[test]
    fn plain_base32_uses_defaults_and_accepts_short_secrets() {
        // 80-bit secret, lowercase with spaces, as sites show them.
        let code = code_at("jbsw y3dp ehpk 3pxp", 59).unwrap();
        assert_eq!(code.code.len(), 6);
        assert_eq!(code.period, 30);
        assert_eq!(code.remaining, 1);
    }

    #[test]
    fn invalid_input_is_rejected() {
        assert!(validate("").is_err());
        assert!(validate("not base32 !!").is_err());
        assert!(validate("otpauth://hotp/x?secret=JBSWY3DPEHPK3PXP").is_err());
        assert!(validate("JBSWY3DPEHPK3PXP").is_ok());
        assert!(validate("otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&period=0").is_err());
        assert!(validate("otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&digits=12").is_err());
        assert!(code_at("otpauth://totp/x?secret=JBSWY3DPEHPK3PXP&period=0", 59).is_err());
    }
}
