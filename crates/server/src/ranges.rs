//! The peer ranges a server admits, when it is not the tailnet rule
//! ([`crate::allowed`]): a list of CIDR blocks, as the hub's config names them
//! (wsp-460). Loopback is no special case here; a config that wants it names
//! `127.0.0.0/8` and `::1/128`.

use std::net::IpAddr;

/// One CIDR block: an address and how many leading bits of it count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Range {
    net: IpAddr,
    bits: u8,
}

impl Range {
    /// `100.64.0.0/10`, `::1/128`; a bare address is a block of one.
    pub fn parse(text: &str) -> Result<Range, String> {
        let (addr, bits) = match text.split_once('/') {
            Some((a, b)) => (a, Some(b)),
            None => (text, None),
        };
        let net: IpAddr = addr.trim().parse().map_err(|e| format!("{text}: not an address: {e}"))?;
        let max = if net.is_ipv4() { 32 } else { 128 };
        let bits = match bits {
            Some(b) => b.trim().parse::<u8>().map_err(|e| format!("{text}: not a prefix length: {e}"))?,
            None => max,
        };
        if bits > max {
            return Err(format!("{text}: a prefix of {bits} bits is longer than {max}"));
        }
        Ok(Range { net, bits })
    }

    /// Whether `ip` is in the block. An IPv4 address mapped into IPv6 is read as
    /// the IPv4 one, as [`crate::allowed`] reads it.
    pub fn contains(&self, ip: &IpAddr) -> bool {
        match (self.net, ip.to_canonical()) {
            (IpAddr::V4(n), IpAddr::V4(a)) => prefix(u32::from(n) as u128, u32::from(a) as u128, 32, self.bits),
            (IpAddr::V6(n), IpAddr::V6(a)) => prefix(u128::from(n), u128::from(a), 128, self.bits),
            _ => false,
        }
    }
}

/// Whether the top `bits` of two `width`-bit numbers agree.
fn prefix(a: u128, b: u128, width: u32, bits: u8) -> bool {
    let bits = bits as u32;
    bits == 0 || (a ^ b) >> (width - bits) == 0
}

/// The blocks a server admits peers from, and may bind addresses in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ranges(Vec<Range>);

impl Ranges {
    /// Every block of `texts`, or the first that does not read.
    pub fn parse<S: AsRef<str>>(texts: &[S]) -> Result<Ranges, String> {
        texts.iter().map(|t| Range::parse(t.as_ref())).collect::<Result<Vec<_>, _>>().map(Ranges)
    }

    pub fn contains(&self, ip: &IpAddr) -> bool {
        self.0.iter().any(|r| r.contains(ip))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn a_block_holds_its_prefix_and_nothing_else() {
        let r = Ranges::parse(&["127.0.0.0/8", "::1/128", "10.1.2.3", "fd00::/8"]).unwrap();
        for yes in ["127.9.9.9", "::1", "10.1.2.3", "fd12::1", "::ffff:127.0.0.1"] {
            assert!(r.contains(&ip(yes)), "{yes}");
        }
        for no in ["10.1.2.4", "128.0.0.1", "::2", "fe80::1", "8.8.8.8"] {
            assert!(!r.contains(&ip(no)), "{no}");
        }
        assert!(Ranges::parse(&["0.0.0.0/0"]).unwrap().contains(&ip("8.8.8.8")));
        assert!(!Ranges::parse(&["0.0.0.0/0"]).unwrap().contains(&ip("::1")), "a family of its own");
        assert!(!Ranges::default().contains(&ip("127.0.0.1")), "no block admits nobody");
    }

    #[test]
    fn a_block_that_does_not_read_is_named() {
        for bad in ["10.0.0.0/33", "::/129", "banana", "10.0.0.0/x", "10.0.0.0/"] {
            assert!(Ranges::parse(&[bad]).is_err(), "{bad}");
        }
    }
}
