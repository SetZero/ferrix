//! A fixed-capacity unsigned big integer: just the operations exact decimal to
//! binary conversion needs.
//!
//! Limbs are 64-bit and little-endian. Every limb at or above `len` is zero,
//! and the limb below `len` is not, so the value zero has `len` 0. Operations
//! that could outgrow the capacity return [`Full`] rather than losing bits; the
//! callers size the capacity so that no valid input reaches it.

use core::cmp::Ordering;

/// An operation needed more limbs than the number has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Full;

/// An unsigned integer of at most `L` 64-bit limbs.
#[derive(Debug)]
pub(crate) struct Big<const L: usize> {
    len: usize,
    limbs: [u64; L],
}

impl<const L: usize> Big<L> {
    /// The number `value`.
    pub(crate) fn from_u64(value: u64) -> Self {
        let mut big = Self {
            len: 0,
            limbs: [0; L],
        };
        if value != 0
            && let Some(first) = big.limbs.first_mut()
        {
            *first = value;
            big.len = 1;
        }
        big
    }

    /// Whether the number is zero.
    pub(crate) const fn is_zero(&self) -> bool {
        self.len == 0
    }

    /// The number of bits up to and including the highest set bit.
    pub(crate) fn bit_len(&self) -> u64 {
        let Some(top) = self.len.checked_sub(1) else {
            return 0;
        };
        let high = self.limbs.get(top).copied().unwrap_or(0);
        top as u64 * 64 + u64::from(64 - high.leading_zeros())
    }

    /// Drops zero limbs from the top.
    fn trim(&mut self) {
        while let Some(top) = self.len.checked_sub(1) {
            if self.limbs.get(top).copied().unwrap_or(0) != 0 {
                break;
            }
            self.len = top;
        }
    }

    /// Sets the number to `self * factor + addend`.
    pub(crate) fn mul_add(&mut self, factor: u64, addend: u64) -> Result<(), Full> {
        let mut carry = u128::from(addend);
        for limb in self.limbs.iter_mut().take(self.len) {
            let wide = u128::from(*limb) * u128::from(factor) + carry;
            // The low half; the high half carries.
            *limb = wide as u64;
            carry = wide >> 64;
        }
        if carry != 0 {
            let slot = self.limbs.get_mut(self.len).ok_or(Full)?;
            // At most `u64::MAX`: both the product's high half and the
            // addend are below 2^64, and so is their sum's high half.
            *slot = carry as u64;
            self.len += 1;
        }
        self.trim();
        Ok(())
    }

    /// Multiplies the number by `10^exponent`.
    pub(crate) fn mul_pow10(&mut self, mut exponent: u64) -> Result<(), Full> {
        /// The largest power of ten in a limb.
        const TEN_19: u64 = 10_000_000_000_000_000_000;
        while exponent >= 19 {
            self.mul_add(TEN_19, 0)?;
            exponent -= 19;
        }
        // Below 19, so the power fits.
        self.mul_add(10_u64.pow(exponent as u32), 0)
    }

    /// Shifts the number left by `bits`.
    pub(crate) fn shl(&mut self, bits: u64) -> Result<(), Full> {
        if self.len == 0 {
            return Ok(());
        }
        let new_bits = self.bit_len().checked_add(bits).ok_or(Full)?;
        if new_bits > L as u64 * 64 {
            return Err(Full);
        }
        // Within the capacity, which is a `usize`.
        let whole = (bits / 64) as usize;
        let part = (bits % 64) as u32;
        let new_len = new_bits.div_ceil(64) as usize;
        // From the top down: a destination limb reads only limbs at or below
        // its own index, which are not yet overwritten.
        let mut d = new_len;
        while d > 0 {
            d -= 1;
            let read = |k: Option<usize>| k.and_then(|k| self.limbs.get(k)).copied().unwrap_or(0);
            let high = read(d.checked_sub(whole));
            let value = if part == 0 {
                high
            } else {
                let low = read(d.checked_sub(whole + 1));
                (high << part) | (low >> (64 - part))
            };
            if let Some(slot) = self.limbs.get_mut(d) {
                *slot = value;
            }
        }
        self.len = new_len;
        self.trim();
        Ok(())
    }

    /// Shifts the number right by one bit.
    pub(crate) fn shr1(&mut self) {
        let mut k = 0;
        while k < self.len {
            let above = self.limbs.get(k + 1).copied().unwrap_or(0);
            if let Some(limb) = self.limbs.get_mut(k) {
                *limb = (*limb >> 1) | (above << 63);
            }
            k += 1;
        }
        self.trim();
    }

    /// Compares two numbers.
    pub(crate) fn compare(&self, other: &Self) -> Ordering {
        if self.len != other.len {
            return self.len.cmp(&other.len);
        }
        let mut k = self.len;
        while k > 0 {
            k -= 1;
            let a = self.limbs.get(k).copied().unwrap_or(0);
            let b = other.limbs.get(k).copied().unwrap_or(0);
            if a != b {
                return a.cmp(&b);
            }
        }
        Ordering::Equal
    }

    /// Subtracts `other`, which must not be greater.
    pub(crate) fn sub(&mut self, other: &Self) {
        let mut borrow = false;
        for (limb, &take) in self.limbs.iter_mut().zip(other.limbs.iter()).take(self.len) {
            let (step, under1) = limb.overflowing_sub(take);
            let (step, under2) = step.overflowing_sub(u64::from(borrow));
            *limb = step;
            borrow = under1 || under2;
        }
        self.trim();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The number's value, for numbers that fit in 128 bits.
    fn value<const L: usize>(big: &Big<L>) -> u128 {
        assert!(big.bit_len() <= 128);
        let low = big.limbs.first().copied().unwrap_or(0);
        let high = big.limbs.get(1).copied().unwrap_or(0);
        u128::from(high) << 64 | u128::from(low)
    }

    #[test]
    fn arithmetic_matches_u128() {
        let mut a = Big::<4>::from_u64(0);
        assert!(a.is_zero());
        a.mul_add(10, 7).unwrap();
        assert_eq!(value(&a), 7);
        a.mul_pow10(20).unwrap();
        assert_eq!(value(&a), 7 * 10_u128.pow(20));
        a.shl(13).unwrap();
        assert_eq!(value(&a), (7 * 10_u128.pow(20)) << 13);
        assert_eq!(
            a.bit_len(),
            128 - ((7 * 10_u128.pow(20)) << 13).leading_zeros() as u64
        );
        a.shr1();
        assert_eq!(value(&a), (7 * 10_u128.pow(20)) << 12);
        let b = Big::<4>::from_u64(u64::MAX);
        assert_eq!(a.compare(&b), Ordering::Greater);
        assert_eq!(b.compare(&a), Ordering::Less);
        a.sub(&b);
        assert_eq!(
            value(&a),
            ((7 * 10_u128.pow(20)) << 12) - u128::from(u64::MAX)
        );
    }

    #[test]
    fn shifts_cross_limbs_and_whole_limbs() {
        let mut a = Big::<4>::from_u64(0x8000_0000_0000_0001);
        a.shl(64).unwrap();
        assert_eq!(value(&a), 0x8000_0000_0000_0001_u128 << 64);
        let mut b = Big::<4>::from_u64(3);
        b.shl(127).unwrap();
        assert_eq!(b.bit_len(), 129);
        let mut c = Big::<2>::from_u64(1);
        assert_eq!(c.shl(128), Err(Full));
        c.shl(127).unwrap();
        assert_eq!(value(&c), 1 << 127);
        assert_eq!(c.mul_add(2, 0), Err(Full));
    }

    #[test]
    fn subtraction_borrows_across_limbs_and_trims() {
        let mut a = Big::<4>::from_u64(1);
        a.shl(128).unwrap();
        let b = Big::<4>::from_u64(1);
        a.sub(&b);
        assert_eq!(a.bit_len(), 128);
        assert_eq!(value(&a), u128::MAX);
        let c = Big::<4>::from_u64(0);
        a.sub(&c);
        assert_eq!(value(&a), u128::MAX);
    }
}
