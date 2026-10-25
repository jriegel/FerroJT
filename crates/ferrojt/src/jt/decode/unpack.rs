//! Residual predictors applied after a codec produced its symbols.
//!
//! Every unpacker works in place and leaves the first four values untouched;
//! predictions read values that have already been unpacked, and all arithmetic
//! wraps like the reference implementation.

/// Which predictor/delta pair to apply.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Unpack {
    /// No transform.
    #[default]
    Null,
    /// Plus the previous value.
    Lag1,
    /// Plus the value two back.
    Lag2,
    /// Xor the previous value.
    Xor1,
    /// Xor the value two back.
    Xor2,
    /// Plus the index.
    Ramp,
    /// Plus a first-order linear extrapolation.
    Stride1,
    /// Plus a second-order linear extrapolation.
    Stride2,
    /// Plus a clamped stride used by triangle strip indices.
    StripIdx,
}

impl Unpack {
    /// Applies the transform to `values` in place.
    pub fn apply(self, values: &mut [i32]) {
        if matches!(self, Unpack::Null) {
            return;
        }
        for index in 4..values.len() {
            let v1 = values[index - 1];
            let v2 = values[index - 2];
            let v4 = values[index - 4];
            let residual = values[index];
            let predicted = match self {
                Unpack::Null => unreachable!("handled above"),
                Unpack::Lag1 => v1,
                Unpack::Lag2 => v2,
                Unpack::Xor1 => v1,
                Unpack::Xor2 => v2,
                Unpack::Ramp => index as i32,
                Unpack::Stride1 => v1.wrapping_add(v1.wrapping_sub(v2)),
                Unpack::Stride2 => v2.wrapping_add(v2.wrapping_sub(v4)),
                Unpack::StripIdx => {
                    let delta = v2.wrapping_sub(v4);
                    v2.wrapping_add(if delta > -8 && delta < 8 { delta } else { 2 })
                }
            };
            values[index] = match self {
                Unpack::Xor1 | Unpack::Xor2 => predicted ^ residual,
                _ => predicted.wrapping_add(residual),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lag1_leaves_the_first_four_values_alone() {
        let mut values = vec![9, 9, 9, 9, 1, 1];
        Unpack::Lag1.apply(&mut values);
        assert_eq!(values, vec![9, 9, 9, 9, 10, 11]);
    }

    #[test]
    fn xor1_uses_the_previous_result() {
        let mut values = vec![0, 0, 0, 5, 0b0110, 0b0011];
        Unpack::Xor1.apply(&mut values);
        assert_eq!(values, vec![0, 0, 0, 5, 3, 0]);
    }

    #[test]
    fn strip_idx_predicts_from_the_stride_two_apart() {
        let mut values = vec![1, 2, 3, 4, 5, 6];
        Unpack::StripIdx.apply(&mut values);
        // index 4: v2 = 3, v4 = 1 -> delta 2 -> 3 + 2 + 5 = 10
        // index 5: v2 = 4, v4 = 2 -> delta 2 -> 4 + 2 + 6 = 12
        assert_eq!(values, vec![1, 2, 3, 4, 10, 12]);
    }

    #[test]
    fn strip_idx_falls_back_to_two_for_wide_deltas() {
        let mut values = vec![0, 0, 100, 0, 0, 7];
        Unpack::StripIdx.apply(&mut values);
        // index 4: v2 = 100, v4 = 0 -> delta 100 is outside (-8, 8) -> 100 + 2
        // index 5: v2 = 0, v4 = 0 -> delta 0 -> 0 + 7
        assert_eq!(values, vec![0, 0, 100, 0, 102, 7]);
    }

    #[test]
    fn ramp_adds_the_index() {
        let mut values = vec![0, 0, 0, 0, 0, 0];
        Unpack::Ramp.apply(&mut values);
        assert_eq!(values, vec![0, 0, 0, 0, 4, 5]);
    }
}
