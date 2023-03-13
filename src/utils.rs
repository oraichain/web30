use base58::{FromBase58, ToBase58};
use sha2::{Digest, Sha256};

pub fn get_evm_address(base58: &str) -> String {
    let bytes = base58.from_base58().expect("Invalid base58 address");
    return format!("0x{}", hex::encode(&bytes[1..bytes.len() - 4]));
}

pub fn get_base58_address(address: &str) -> String {
    let mut evm_address = vec![0x41];
    evm_address.extend(
        hex::decode(address.strip_prefix("0x").unwrap_or(address)).expect("Invalid hex address"),
    );
    let mut hasher = Sha256::new();
    hasher.update(evm_address.clone());
    let digest1 = hasher.finalize();

    let mut hasher = Sha256::new();
    hasher.update(&digest1);
    let digest = hasher.finalize();

    evm_address.extend(&digest[..4]);
    return evm_address.to_base58();
}

#[test]
fn convert_address() {
    let evm_address = get_evm_address("TY5X9ocQACH9YGAyiK3WUxLcLw3t2ethnc");
    let base58_address = get_base58_address("0xf2846a1e4dafaea38c1660a618277d67605bd2b5");
    assert_eq!(evm_address, "0xf2846a1e4dafaea38c1660a618277d67605bd2b5");
    assert_eq!(base58_address, "TY5X9ocQACH9YGAyiK3WUxLcLw3t2ethnc");
}
