//! Strong random passwords.

use super::crypto::{CryptoError, random_bytes};

pub const MIN_LENGTH: usize = 8;
pub const MAX_LENGTH: usize = 64;
const UPPER: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZ";
const LOWER: &str = "abcdefghijklmnopqrstuvwxyz";
const DIGITS: &str = "0123456789";
pub(crate) const SYMBOLS: &str = "!@#$%^&*()-_=+[]{};:,.?/";
pub(crate) const AMBIGUOUS: &str = "0O1lI";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GeneratorOptions {
    pub length: usize,
    pub upper: bool,
    pub lower: bool,
    pub digits: bool,
    pub symbols: bool,
    pub exclude_ambiguous: bool,
}

impl Default for GeneratorOptions {
    fn default() -> Self {
        GeneratorOptions {
            length: 20,
            upper: true,
            lower: true,
            digits: true,
            symbols: true,
            exclude_ambiguous: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeneratorError {
    NoClasses,
    Random,
}

impl From<CryptoError> for GeneratorError {
    fn from(_: CryptoError) -> Self {
        GeneratorError::Random
    }
}

/// A uniform random number below `n` (> 0), without modulo bias.
pub(crate) fn random_below(n: u32) -> Result<u32, GeneratorError> {
    let zone = u32::MAX - (u32::MAX % n);
    loop {
        let r = u32::from_le_bytes(random_bytes::<4>()?);
        if r < zone {
            return Ok(r % n);
        }
    }
}

fn pick(chars: &[char]) -> Result<char, GeneratorError> {
    Ok(chars[random_below(chars.len() as u32)? as usize])
}

/// A password with at least one character of each chosen class.
pub fn generate(options: &GeneratorOptions) -> Result<String, GeneratorError> {
    let classes: Vec<Vec<char>> = [
        (options.upper, UPPER),
        (options.lower, LOWER),
        (options.digits, DIGITS),
        (options.symbols, SYMBOLS),
    ]
    .into_iter()
    .filter(|(on, _)| *on)
    .map(|(_, set)| {
        set.chars()
            .filter(|c| !(options.exclude_ambiguous && AMBIGUOUS.contains(*c)))
            .collect()
    })
    .collect();
    if classes.is_empty() {
        return Err(GeneratorError::NoClasses);
    }
    let length = options.length.clamp(MIN_LENGTH, MAX_LENGTH);
    let all: Vec<char> = classes.concat();
    let mut chars: Vec<char> = classes
        .iter()
        .map(|class| pick(class))
        .collect::<Result<_, _>>()?;
    while chars.len() < length {
        chars.push(pick(&all)?);
    }
    for i in (1..chars.len()).rev() {
        let j = random_below(i as u32 + 1)? as usize;
        chars.swap(i, j);
    }
    Ok(chars.into_iter().collect())
}

/// How strong a master password looks: 0 too short, 1 weak, 2 fair, 3 strong.
pub fn strength(password: &str) -> u8 {
    let length = password.chars().count();
    if length < super::MIN_PASSWORD_CHARS {
        return 0;
    }
    let classes = [
        password.chars().any(|c| c.is_lowercase()),
        password.chars().any(|c| c.is_uppercase()),
        password.chars().any(|c| c.is_ascii_digit()),
        password.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|on| **on)
    .count();
    match (length, classes) {
        (16.., 3..) | (20.., _) => 3,
        (_, 3..) | (14.., _) => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn strength_grows_with_length_and_variety() {
        assert_eq!(strength("short"), 0);
        assert_eq!(strength("abcdefghij"), 1);
        assert_eq!(strength("abcdefghij12!"), 2);
        assert_eq!(strength("Abcdefghij12!xyz"), 3);
        assert_eq!(strength("correct horse battery staple"), 3);
    }

    use super::*;

    #[test]
    fn default_password_has_every_class() {
        let pw = generate(&GeneratorOptions::default()).unwrap();
        assert_eq!(pw.chars().count(), 20);
        assert!(pw.chars().any(|c| c.is_ascii_uppercase()));
        assert!(pw.chars().any(|c| c.is_ascii_lowercase()));
        assert!(pw.chars().any(|c| c.is_ascii_digit()));
        assert!(pw.chars().any(|c| SYMBOLS.contains(c)));
        assert!(!pw.chars().any(|c| AMBIGUOUS.contains(c)));
    }

    #[test]
    fn length_is_clamped() {
        let short = generate(&GeneratorOptions {
            length: 2,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(short.len(), MIN_LENGTH);
        let long = generate(&GeneratorOptions {
            length: 500,
            ..Default::default()
        })
        .unwrap();
        assert_eq!(long.len(), MAX_LENGTH);
    }

    #[test]
    fn single_class_and_no_class() {
        let opts = GeneratorOptions {
            upper: false,
            lower: false,
            symbols: false,
            ..Default::default()
        };
        assert!(generate(&opts).unwrap().chars().all(|c| c.is_ascii_digit()));
        let none = GeneratorOptions {
            upper: false,
            lower: false,
            digits: false,
            symbols: false,
            ..Default::default()
        };
        assert_eq!(generate(&none).unwrap_err(), GeneratorError::NoClasses);
    }

    #[test]
    fn random_below_is_in_range_and_roughly_uniform() {
        let mut counts = [0_u32; 7];
        for _ in 0..7000 {
            counts[random_below(7).unwrap() as usize] += 1;
        }
        assert!(
            counts.iter().all(|&n| (700..1300).contains(&n)),
            "{counts:?}"
        );
    }
}
