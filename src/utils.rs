use alloy::primitives::{B256, PrimitiveSignature};

/// Extracts the components of a signature.
/// Currently alloy has no function for extracting the components of a signature.
/// Returns a tuple of (v, r, s) where:
/// - `v` is the recovery id
/// - `r` is the r component of the signature
/// - `s` is the s component of the signature
pub fn extract_signature_components(sig: &PrimitiveSignature) -> (u8, B256, B256) {
    let v = sig.as_bytes()[64];
    let r = sig.r().into();
    let s = sig.s().into();
    (v, r, s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::U256;

    #[test]
    fn test_extract_signature_components() {
        let r = U256::from(123456789);
        let s = U256::from(987654321);
        let y_parity = true;
        let sig = PrimitiveSignature::new(r, s, y_parity);
        let (v, r_bytes, s_bytes) = extract_signature_components(&sig);
        assert_eq!(v, 28);
        assert_eq!(U256::from_be_bytes(r_bytes.0), r);
        assert_eq!(U256::from_be_bytes(s_bytes.0), s);
    }
}
