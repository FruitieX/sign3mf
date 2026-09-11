use anyhow::{anyhow, Result};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn distance(self, other: Rgb) -> u8 {
        self.0
            .abs_diff(other.0)
            .max(self.1.abs_diff(other.1))
            .max(self.2.abs_diff(other.2))
    }
    pub fn hex(self) -> String {
        format!("{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
    pub fn html(self) -> String {
        format!("#{}", self.hex())
    }
}

impl fmt::Display for Rgb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.hex())
    }
}

pub fn parse_hex(s: &str) -> Result<Rgb> {
    let clean = s.trim().trim_start_matches('#');
    if clean.len() != 6 || !clean.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(anyhow!("invalid color '{s}'; expected #RRGGBB"));
    }
    let n = u32::from_str_radix(clean, 16).map_err(|_| anyhow!("invalid color '{s}'"))?;
    Ok(Rgb((n >> 16) as u8, (n >> 8) as u8, n as u8))
}

pub fn parse_palette(s: &str) -> Result<Vec<Rgb>> {
    let colors: Result<Vec<_>> = s.split(',').map(parse_hex).collect();
    let colors = colors?;
    if colors.is_empty() {
        return Err(anyhow!("palette cannot be empty"));
    }
    Ok(colors)
}
