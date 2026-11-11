//! Native measured quantity, following MONEY's minor-units/scale/unit pattern.

use core::{
    cmp::Ordering,
    hash::{Hash, Hasher},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum TemperatureUnit {
    Celsius = 0,
    Fahrenheit = 1,
    Kelvin = 2,
}

impl TemperatureUnit {
    pub fn from_byte(byte: u8) -> Option<Self> {
        match byte {
            0 => Some(Self::Celsius),
            1 => Some(Self::Fahrenheit),
            2 => Some(Self::Kelvin),
            _ => None,
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "C" | "°C" => Some(Self::Celsius),
            "F" | "°F" => Some(Self::Fahrenheit),
            "K" => Some(Self::Kelvin),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Celsius => "C",
            Self::Fahrenheit => "F",
            Self::Kelvin => "K",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Temperature {
    minor_units: i64,
    scale: u8,
    unit: TemperatureUnit,
}

impl Temperature {
    pub fn new(minor_units: i64, scale: u8, unit: TemperatureUnit) -> Result<Self, &'static str> {
        if scale > 18 {
            return Err("temperature scale exceeds 18");
        }
        let value = Self {
            minor_units,
            scale,
            unit,
        };
        if value.kelvin_fraction().0 < 0 {
            return Err("temperature is below absolute zero");
        }
        Ok(value)
    }

    pub fn minor_units(self) -> i64 {
        self.minor_units
    }

    pub fn scale(self) -> u8 {
        self.scale
    }

    pub fn unit(self) -> TemperatureUnit {
        self.unit
    }

    /// Exact rational Kelvin. Decimal input and affine unit offsets stay exact.
    pub fn kelvin_fraction(self) -> (i128, i128) {
        let base = 10i128.pow(u32::from(self.scale));
        let amount = i128::from(self.minor_units);
        match self.unit {
            TemperatureUnit::Kelvin => (amount, base),
            TemperatureUnit::Celsius => (amount * 100 + 27315 * base, 100 * base),
            TemperatureUnit::Fahrenheit => (amount * 500 + 229835 * base, 900 * base),
        }
    }

    pub fn value_in(self, unit: TemperatureUnit) -> f64 {
        if unit == self.unit {
            return self.minor_units as f64 / 10f64.powi(i32::from(self.scale));
        }
        let (numerator, denominator) = self.kelvin_fraction();
        let kelvin = numerator as f64 / denominator as f64;
        match unit {
            TemperatureUnit::Kelvin => kelvin,
            TemperatureUnit::Celsius => kelvin - 273.15,
            TemperatureUnit::Fahrenheit => (kelvin - 273.15) * 1.8 + 32.0,
        }
    }

    fn normalized(self) -> (i128, i128) {
        let (numerator, denominator) = self.kelvin_fraction();
        let (mut a, mut b) = (numerator, denominator);
        while b != 0 {
            (a, b) = (b, a % b);
        }
        (numerator / a, denominator / a)
    }
}

impl PartialEq for Temperature {
    fn eq(&self, other: &Self) -> bool {
        self.normalized() == other.normalized()
    }
}

impl Eq for Temperature {}

impl Hash for Temperature {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.normalized().hash(state);
    }
}

impl PartialOrd for Temperature {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Temperature {
    fn cmp(&self, other: &Self) -> Ordering {
        let (mut a, mut b) = self.kelvin_fraction();
        let (mut c, mut d) = other.kelvin_fraction();
        let mut reverse = false;
        // Continued fractions avoid overflowing i128 cross-products at scale 18.
        loop {
            let order = (a / b).cmp(&(c / d));
            if order != Ordering::Equal {
                return if reverse { order.reverse() } else { order };
            }
            let (r, s) = (a % b, c % d);
            if r == 0 || s == 0 {
                let order = r.cmp(&s);
                return if reverse { order.reverse() } else { order };
            }
            (a, b, c, d) = (b, r, d, s);
            reverse = !reverse;
        }
    }
}

impl core::fmt::Display for Temperature {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "{} {}",
            crate::types::format_scaled_i64(self.minor_units, self.scale),
            self.unit.as_str()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DataType, Value};
    use core::hash::{Hash, Hasher};

    #[derive(Default)]
    struct TestHasher(u64);

    impl Hasher for TestHasher {
        fn finish(&self) -> u64 {
            self.0
        }

        fn write(&mut self, bytes: &[u8]) {
            for byte in bytes {
                self.0 = self.0.wrapping_mul(31).wrapping_add(u64::from(*byte));
            }
        }
    }

    #[test]
    fn exact_units_absolute_zero_and_extreme_scale() {
        let c = Temperature::new(20, 0, TemperatureUnit::Celsius).expect("valid temperature");
        let f = Temperature::new(68, 0, TemperatureUnit::Fahrenheit).expect("valid temperature");
        let k = Temperature::new(29315, 2, TemperatureUnit::Kelvin).expect("valid temperature");
        assert_eq!(c, f);
        assert_eq!(c, k);
        assert_eq!(c.cmp(&f), Ordering::Equal);
        assert!(Temperature::new(-27316, 2, TemperatureUnit::Celsius).is_err());
        assert_eq!(
            Temperature::new(-45967, 2, TemperatureUnit::Fahrenheit).expect("valid temperature"),
            Temperature::new(0, 0, TemperatureUnit::Kelvin).expect("valid temperature")
        );
        assert!(
            Temperature::new(i64::MAX, 18, TemperatureUnit::Celsius).expect("valid temperature")
                > Temperature::new(i64::MIN, 18, TemperatureUnit::Celsius)
                    .expect("valid temperature")
        );
        assert_eq!(c.to_string(), "20 C");
    }

    #[test]
    fn codec_preserves_units_and_rejects_invalid_payloads() {
        assert_eq!(DataType::Temperature.to_byte(), 56);
        for unit in [
            TemperatureUnit::Celsius,
            TemperatureUnit::Fahrenheit,
            TemperatureUnit::Kelvin,
        ] {
            let value = Temperature::new(123456, 3, unit).expect("valid temperature");
            let bytes = Value::Temperature(value).to_bytes();
            assert_eq!(bytes.len(), 11);
            let (Value::Temperature(decoded), used) = Value::from_bytes(&bytes).expect("decode")
            else {
                panic!("type lost")
            };
            assert_eq!(used, 11);
            assert_eq!(
                (decoded.minor_units(), decoded.scale(), decoded.unit()),
                (value.minor_units(), value.scale(), unit)
            );
            for length in 0..11 {
                assert!(Value::from_bytes(&bytes[..length]).is_err());
            }
            for (offset, invalid) in [(1, 3), (2, 19)] {
                let mut corrupt = bytes.clone();
                corrupt[offset] = invalid;
                assert!(Value::from_bytes(&corrupt).is_err());
            }
        }
        let mut negative = vec![56, 2, 0];
        negative.extend_from_slice(&(-1i64).to_le_bytes());
        assert!(Value::from_bytes(&negative).is_err());
    }

    proptest::proptest! {
        #[test]
        fn equivalent_units_have_the_same_order_and_hash(centi in -27315i64..=1_000_000_000) {
            let c = Temperature::new(centi, 2, TemperatureUnit::Celsius).expect("valid");
            let f = Temperature::new(centi * 18 + 32000, 3, TemperatureUnit::Fahrenheit)
                .expect("valid");
            let k = Temperature::new(centi + 27315, 2, TemperatureUnit::Kelvin).expect("valid");
            proptest::prop_assert_eq!(c, f);
            proptest::prop_assert_eq!(c, k);
            proptest::prop_assert_eq!(c.cmp(&f), core::cmp::Ordering::Equal);
            let mut a = TestHasher::default();
            let mut b = TestHasher::default();
            c.hash(&mut a);
            f.hash(&mut b);
            proptest::prop_assert_eq!(a.finish(), b.finish());
        }
    }
}
