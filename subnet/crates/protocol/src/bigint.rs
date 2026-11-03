//! Minimal exact unsigned integers and fractions for `reward-v1` credits.
//!
//! Geometric credits with `r = 17/20` over many bands have denominators far
//! beyond `u128`, so the band split runs on these arbitrary-precision values.
//! They use only integer limb arithmetic, so every target computes identical
//! results. The crate carries its own implementation rather than a dependency
//! so the shared protocol code keeps its existing dependency set.

use core::cmp::Ordering;
use core::fmt;

use crate::codec::{Canonical, Decoder, Encoder};
use crate::{Error, Result};

/// Most limbs a recorded integer may carry: 4,096 bits. An exact `reward-v1`
/// credit over a 256-entrant roster needs fewer than 1,120 bits.
pub const MAX_LIMBS: usize = 128;

/// Arbitrary-precision unsigned integer: little-endian `u32` limbs with no
/// most-significant zero limb, so equal values have equal representations.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct BigUint {
    limbs: Vec<u32>,
}

/// A `u32` limb count, then the limbs as little-endian `u32`, least
/// significant first, with no most-significant zero limb. Zero has no limbs.
impl Canonical for BigUint {
    fn encode_to(&self, e: &mut Encoder) {
        e.len_prefix(self.limbs.len());
        for &limb in &self.limbs {
            e.u32(limb);
        }
    }
    fn decode_from(d: &mut Decoder<'_>) -> Result<Self> {
        let len = d.len_prefix("big integer", MAX_LIMBS as u32)?;
        let mut limbs = Vec::with_capacity(len.min(d.remaining() / 4));
        for _ in 0..len {
            limbs.push(d.u32()?);
        }
        if limbs.last() == Some(&0) {
            return Err(Error::NonCanonical);
        }
        Ok(Self { limbs })
    }
}

impl BigUint {
    /// Fails with `length_limit` above [`MAX_LIMBS`], which a record could
    /// encode but never decode.
    pub fn check_recordable(&self, field: &'static str) -> Result<()> {
        if self.limbs.len() > MAX_LIMBS {
            return Err(Error::LengthLimit {
                field,
                len: self.limbs.len() as u64,
            });
        }
        Ok(())
    }

    pub fn zero() -> Self {
        Self::default()
    }

    pub fn one() -> Self {
        Self::from_u128(1)
    }

    pub fn from_u128(mut value: u128) -> Self {
        let mut limbs = Vec::new();
        while value != 0 {
            limbs.push(value as u32);
            value >>= 32;
        }
        Self { limbs }
    }

    pub fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    fn normalized(mut limbs: Vec<u32>) -> Self {
        while limbs.last() == Some(&0) {
            limbs.pop();
        }
        Self { limbs }
    }

    /// The value, when it fits in `u128`.
    pub fn to_u128(&self) -> Option<u128> {
        if self.limbs.len() > 4 {
            return None;
        }
        Some(
            self.limbs
                .iter()
                .rev()
                .fold(0u128, |acc, &limb| (acc << 32) | u128::from(limb)),
        )
    }

    /// Number of significant bits.
    pub fn bits(&self) -> u64 {
        match self.limbs.last() {
            None => 0,
            Some(top) => (self.limbs.len() as u64 - 1) * 32 + u64::from(32 - top.leading_zeros()),
        }
    }

    fn bit(&self, index: u64) -> bool {
        let limb = (index / 32) as usize;
        self.limbs
            .get(limb)
            .is_some_and(|l| (l >> (index % 32)) & 1 == 1)
    }

    pub fn add(&self, other: &Self) -> Self {
        let (long, short) = if self.limbs.len() >= other.limbs.len() {
            (self, other)
        } else {
            (other, self)
        };
        let mut out = Vec::with_capacity(long.limbs.len() + 1);
        let mut carry = 0u64;
        for (i, &limb) in long.limbs.iter().enumerate() {
            let sum = u64::from(limb) + u64::from(*short.limbs.get(i).unwrap_or(&0)) + carry;
            out.push(sum as u32);
            carry = sum >> 32;
        }
        if carry != 0 {
            out.push(carry as u32);
        }
        Self::normalized(out)
    }

    /// `self - other`, or `None` when `other` is larger.
    pub fn checked_sub(&self, other: &Self) -> Option<Self> {
        if *self < *other {
            return None;
        }
        let mut out = Vec::with_capacity(self.limbs.len());
        let mut borrow = 0i64;
        for (i, &limb) in self.limbs.iter().enumerate() {
            let mut diff = i64::from(limb) - i64::from(*other.limbs.get(i).unwrap_or(&0)) - borrow;
            borrow = 0;
            if diff < 0 {
                diff += 1 << 32;
                borrow = 1;
            }
            out.push(diff as u32);
        }
        Some(Self::normalized(out))
    }

    pub fn mul(&self, other: &Self) -> Self {
        if self.is_zero() || other.is_zero() {
            return Self::zero();
        }
        let mut out = vec![0u32; self.limbs.len() + other.limbs.len()];
        for (i, &a) in self.limbs.iter().enumerate() {
            let mut carry = 0u64;
            for (j, &b) in other.limbs.iter().enumerate() {
                // (2^32 - 1)^2 + 2 (2^32 - 1) = 2^64 - 1 fits.
                let t = u64::from(a) * u64::from(b) + u64::from(out[i + j]) + carry;
                out[i + j] = t as u32;
                carry = t >> 32;
            }
            let mut k = i + other.limbs.len();
            while carry != 0 {
                let t = u64::from(out[k]) + carry;
                out[k] = t as u32;
                carry = t >> 32;
                k += 1;
            }
        }
        Self::normalized(out)
    }

    pub fn mul_u64(&self, factor: u64) -> Self {
        let mut out = Vec::with_capacity(self.limbs.len() + 2);
        let mut carry = 0u128;
        for &limb in &self.limbs {
            let t = u128::from(limb) * u128::from(factor) + carry;
            out.push(t as u32);
            carry = t >> 32;
        }
        while carry != 0 {
            out.push(carry as u32);
            carry >>= 32;
        }
        Self::normalized(out)
    }

    /// `base^exp` by repeated multiplication.
    pub fn pow_u64(base: u64, exp: u32) -> Self {
        let mut out = Self::one();
        for _ in 0..exp {
            out = out.mul_u64(base);
        }
        out
    }

    /// Quotient and remainder by a small divisor.
    pub fn div_rem_u32(&self, divisor: u32) -> (Self, u32) {
        assert!(divisor != 0, "division by zero");
        let mut out = vec![0u32; self.limbs.len()];
        let mut rem = 0u64;
        for i in (0..self.limbs.len()).rev() {
            let cur = (rem << 32) | u64::from(self.limbs[i]);
            out[i] = (cur / u64::from(divisor)) as u32;
            rem = cur % u64::from(divisor);
        }
        (Self::normalized(out), rem as u32)
    }

    /// Quotient and remainder by binary long division. `None` for a zero divisor.
    pub fn div_rem(&self, divisor: &Self) -> Option<(Self, Self)> {
        if divisor.is_zero() {
            return None;
        }
        if self < divisor {
            return Some((Self::zero(), self.clone()));
        }
        let mut quotient = vec![0u32; self.limbs.len()];
        let mut rem = Self::zero();
        for index in (0..self.bits()).rev() {
            rem = rem.shl1(self.bit(index));
            if rem >= *divisor {
                rem = rem.checked_sub(divisor).expect("rem >= divisor");
                quotient[(index / 32) as usize] |= 1 << (index % 32);
            }
        }
        Some((Self::normalized(quotient), rem))
    }

    fn shl1(&self, low_bit: bool) -> Self {
        let mut out = Vec::with_capacity(self.limbs.len() + 1);
        let mut carry = u32::from(low_bit);
        for &limb in &self.limbs {
            out.push((limb << 1) | carry);
            carry = limb >> 31;
        }
        if carry != 0 {
            out.push(carry);
        }
        Self::normalized(out)
    }

    /// Greatest common divisor by Euclid's algorithm.
    pub fn gcd(&self, other: &Self) -> Self {
        let (mut a, mut b) = (self.clone(), other.clone());
        while !b.is_zero() {
            let (_, r) = a.div_rem(&b).expect("nonzero divisor");
            a = b;
            b = r;
        }
        a
    }
}

impl Ord for BigUint {
    fn cmp(&self, other: &Self) -> Ordering {
        self.limbs
            .len()
            .cmp(&other.limbs.len())
            .then_with(|| self.limbs.iter().rev().cmp(other.limbs.iter().rev()))
    }
}

impl PartialOrd for BigUint {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for BigUint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_zero() {
            return f.write_str("0");
        }
        let mut chunks = Vec::new();
        let mut rest = self.clone();
        while !rest.is_zero() {
            let (q, r) = rest.div_rem_u32(1_000_000_000);
            chunks.push(r);
            rest = q;
        }
        let mut out = format!("{}", chunks.pop().expect("nonzero"));
        for chunk in chunks.iter().rev() {
            out.push_str(&format!("{chunk:09}"));
        }
        f.write_str(&out)
    }
}

impl fmt::Debug for BigUint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

/// A nonnegative exact fraction. Values are compared exactly and need not be
/// reduced; [`Fraction::reduced`] gives the lowest-terms form for display.
#[derive(Clone, Debug)]
pub struct Fraction {
    pub num: BigUint,
    pub den: BigUint,
}

impl Fraction {
    /// Fails closed on a zero denominator.
    pub fn new(num: BigUint, den: BigUint) -> Option<Self> {
        (!den.is_zero()).then_some(Self { num, den })
    }

    pub fn reduced(&self) -> Self {
        let g = self.num.gcd(&self.den);
        if g.is_zero() {
            return self.clone();
        }
        Self {
            num: self.num.div_rem(&g).expect("nonzero gcd").0,
            den: self.den.div_rem(&g).expect("nonzero gcd").0,
        }
    }

    pub fn add(&self, other: &Self) -> Self {
        Self {
            num: self.num.mul(&other.den).add(&other.num.mul(&self.den)),
            den: self.den.mul(&other.den),
        }
    }

    /// `floor(self * scale)`, for display in fixed decimal units.
    pub fn floor_scaled(&self, scale: u64) -> BigUint {
        self.num
            .mul_u64(scale)
            .div_rem(&self.den)
            .expect("nonzero denominator")
            .0
    }
}

impl PartialEq for Fraction {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for Fraction {}

impl Ord for Fraction {
    fn cmp(&self, other: &Self) -> Ordering {
        self.num.mul(&other.den).cmp(&other.num.mul(&self.den))
    }
}

impl PartialOrd for Fraction {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Fraction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.num, self.den)
    }
}
