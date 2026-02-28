//! Hyperliquid message signing.
//!
//! Hyperliquid uses EIP-712 typed data signing for L1 actions.
//! The flow is:
//! 1. Serialize action using msgpack
//! 2. Append nonce + vault_address + expires_after
//! 3. Keccak256 hash the result
//! 4. Create phantom agent with source ("a" for mainnet, "b" for testnet)
//! 5. Sign using EIP-712 typed data

use ethers_core::types::{
    transaction::eip712::EIP712Domain,
    Address, Signature, H256, U256,
};
use ethers_signers::{LocalWallet, Signer};
use sha3::{Digest, Keccak256};
use thiserror::Error;
use tracing;

#[derive(Debug, Error)]
pub enum SigningError {
    #[error("Invalid private key: {0}")]
    InvalidPrivateKey(String),
    #[error("Signing failed: {0}")]
    SigningFailed(String),
    #[error("Msgpack encoding failed: {0}")]
    MsgpackError(String),
}

/// Signature components for Hyperliquid API
#[derive(Debug, Clone)]
pub struct HyperliquidSignature {
    pub r: String,
    pub s: String,
    pub v: u8,
}

impl HyperliquidSignature {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "r": self.r,
            "s": self.s,
            "v": self.v
        })
    }
}

/// Hyperliquid signer that handles EIP-712 signing
pub struct HyperliquidSigner {
    wallet: LocalWallet,
    pub address: Address,
    is_mainnet: bool,
}

impl HyperliquidSigner {
    /// Create a new signer from a private key hex string
    pub fn new(private_key: &str, is_mainnet: bool) -> Result<Self, SigningError> {
        // Remove 0x prefix if present
        let key = private_key.strip_prefix("0x").unwrap_or(private_key);

        let wallet: LocalWallet = key
            .parse()
            .map_err(|e| SigningError::InvalidPrivateKey(format!("{}", e)))?;

        let address = wallet.address();

        Ok(Self {
            wallet,
            address,
            is_mainnet,
        })
    }

    /// Get the source string for phantom agent
    fn source(&self) -> &'static str {
        if self.is_mainnet {
            "a"
        } else {
            "b"
        }
    }

    /// Compute action hash for L1 actions
    /// action_hash = keccak256(msgpack(action) + nonce_bytes + vault_flag + vault_address? + expires_flag + expires_bytes?)
    pub fn action_hash(
        &self,
        action: &serde_json::Value,
        nonce: u64,
        vault_address: Option<&str>,
        expires_after: Option<u64>,
    ) -> Result<H256, SigningError> {
        // Serialize action to msgpack (use to_vec for compatibility with Python msgpack.packb)
        let msgpack_data = rmp_serde::to_vec(action)
            .map_err(|e| SigningError::MsgpackError(e.to_string()))?;

        tracing::info!("=== ACTION HASH DEBUG ===");
        tracing::info!(
            "Action JSON: {}",
            serde_json::to_string(action).unwrap_or_default()
        );
        tracing::info!("Msgpack bytes ({} bytes): {}", msgpack_data.len(), hex::encode(&msgpack_data));

        let mut data = msgpack_data;

        // Append nonce as big-endian u64
        data.extend_from_slice(&nonce.to_be_bytes());

        // Append vault address flag and address
        if let Some(vault) = vault_address {
            data.push(0x01);
            let vault_bytes = address_to_bytes(vault)?;
            data.extend_from_slice(&vault_bytes);
        } else {
            data.push(0x00);
        }

        // Append expires_after if present
        if let Some(expires) = expires_after {
            data.push(0x00);
            data.extend_from_slice(&expires.to_be_bytes());
        }

        tracing::info!("Nonce: {}", nonce);
        tracing::info!("Nonce bytes: {}", hex::encode(&nonce.to_be_bytes()));
        tracing::info!("Vault address: {:?}", vault_address);
        tracing::info!("Full data to hash ({} bytes): {}", data.len(), hex::encode(&data));

        // Keccak256 hash
        let mut hasher = Keccak256::new();
        hasher.update(&data);
        let hash = hasher.finalize();

        let hash_result = H256::from_slice(&hash);
        tracing::info!("Action hash (connectionId): 0x{}", hex::encode(hash_result.as_bytes()));

        Ok(hash_result)
    }

    /// Sign an L1 action (orders, cancels, etc.)
    pub async fn sign_l1_action(
        &self,
        action: &serde_json::Value,
        nonce: u64,
        vault_address: Option<&str>,
        expires_after: Option<u64>,
    ) -> Result<HyperliquidSignature, SigningError> {
        tracing::debug!("Signing L1 action with signer address: {:?}", self.address);
        tracing::debug!("Nonce: {}, vault_address: {:?}, is_mainnet: {}", nonce, vault_address, self.is_mainnet);

        let hash = self.action_hash(action, nonce, vault_address, expires_after)?;

        // Create phantom agent
        let phantom_agent = PhantomAgent {
            source: self.source().to_string(),
            connection_id: hash,
        };

        tracing::debug!("Phantom agent source: '{}', connectionId: 0x{}", phantom_agent.source, hex::encode(phantom_agent.connection_id.as_bytes()));

        // Sign using EIP-712
        self.sign_phantom_agent(&phantom_agent).await
    }

    /// Sign a phantom agent using EIP-712 typed data
    async fn sign_phantom_agent(
        &self,
        phantom_agent: &PhantomAgent,
    ) -> Result<HyperliquidSignature, SigningError> {
        // EIP-712 domain for Hyperliquid
        let domain = EIP712Domain {
            name: Some("Exchange".to_string()),
            version: Some("1".to_string()),
            chain_id: Some(U256::from(1337)),
            verifying_contract: Some(Address::zero()),
            salt: None,
        };

        let domain_separator = compute_domain_separator(&domain);
        tracing::info!("=== EIP-712 SIGNING DEBUG ===");
        tracing::info!("Domain separator: 0x{}", hex::encode(&domain_separator));

        // type_hash = keccak256("Agent(string source,bytes32 connectionId)")
        let type_hash = keccak256(b"Agent(string source,bytes32 connectionId)");
        tracing::info!("Type hash: 0x{}", hex::encode(&type_hash));

        // struct_hash = keccak256(abi.encode(type_hash, keccak256(source), connectionId))
        let source_hash = keccak256(phantom_agent.source.as_bytes());
        tracing::info!("Source: '{}', source_hash: 0x{}", phantom_agent.source, hex::encode(&source_hash));
        tracing::info!("ConnectionId: 0x{}", hex::encode(phantom_agent.connection_id.as_bytes()));

        let mut struct_data = Vec::new();
        struct_data.extend_from_slice(&type_hash);
        struct_data.extend_from_slice(&source_hash);
        struct_data.extend_from_slice(phantom_agent.connection_id.as_bytes());
        let struct_hash = keccak256(&struct_data);
        tracing::info!("Struct data ({} bytes): 0x{}", struct_data.len(), hex::encode(&struct_data));
        tracing::info!("Struct hash: 0x{}", hex::encode(&struct_hash));

        // Final hash = keccak256("\x19\x01" + domain_separator + struct_hash)
        let mut final_data = Vec::new();
        final_data.push(0x19);
        final_data.push(0x01);
        final_data.extend_from_slice(&domain_separator);
        final_data.extend_from_slice(&struct_hash);
        let final_hash = keccak256(&final_data);
        tracing::info!("Final hash to sign: 0x{}", hex::encode(&final_hash));

        // Sign the hash
        let signature = self
            .wallet
            .sign_hash(H256::from_slice(&final_hash))
            .map_err(|e| SigningError::SigningFailed(e.to_string()))?;

        Ok(signature_to_hyperliquid(&signature))
    }

    /// Get the wallet address as a hex string
    pub fn address_string(&self) -> String {
        format!("{:?}", self.address)
    }
}

/// Phantom agent structure for L1 action signing
struct PhantomAgent {
    source: String,
    connection_id: H256,
}

/// Convert address string to bytes
fn address_to_bytes(address: &str) -> Result<[u8; 20], SigningError> {
    let addr = address.strip_prefix("0x").unwrap_or(address);
    let bytes = hex::decode(addr).map_err(|e| SigningError::InvalidPrivateKey(e.to_string()))?;
    if bytes.len() != 20 {
        return Err(SigningError::InvalidPrivateKey(format!(
            "Invalid address length: {}",
            bytes.len()
        )));
    }
    let mut result = [0u8; 20];
    result.copy_from_slice(&bytes);
    Ok(result)
}

/// Compute keccak256 hash
fn keccak256(data: &[u8]) -> [u8; 32] {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    let result = hasher.finalize();
    let mut hash = [0u8; 32];
    hash.copy_from_slice(&result);
    hash
}

/// Compute EIP-712 domain separator
fn compute_domain_separator(domain: &EIP712Domain) -> [u8; 32] {
    // type_hash = keccak256("EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)")
    let type_hash = keccak256(
        b"EIP712Domain(string name,string version,uint256 chainId,address verifyingContract)",
    );

    let name_hash = keccak256(domain.name.as_ref().unwrap().as_bytes());
    let version_hash = keccak256(domain.version.as_ref().unwrap().as_bytes());

    let mut chain_id_bytes = [0u8; 32];
    domain
        .chain_id
        .unwrap()
        .to_big_endian(&mut chain_id_bytes);

    let verifying_contract = domain.verifying_contract.unwrap();
    let mut contract_bytes = [0u8; 32];
    contract_bytes[12..].copy_from_slice(verifying_contract.as_bytes());

    // abi.encode(type_hash, name_hash, version_hash, chain_id, verifying_contract)
    let mut encoded = Vec::new();
    encoded.extend_from_slice(&type_hash);
    encoded.extend_from_slice(&name_hash);
    encoded.extend_from_slice(&version_hash);
    encoded.extend_from_slice(&chain_id_bytes);
    encoded.extend_from_slice(&contract_bytes);

    keccak256(&encoded)
}

/// Convert ethers Signature to HyperliquidSignature
fn signature_to_hyperliquid(sig: &Signature) -> HyperliquidSignature {
    let r_bytes: [u8; 32] = sig.r.into();
    let s_bytes: [u8; 32] = sig.s.into();

    let result = HyperliquidSignature {
        r: format!("0x{}", hex::encode(r_bytes)),
        s: format!("0x{}", hex::encode(s_bytes)),
        v: sig.v as u8,
    };

    tracing::debug!("Signature: r={}, s={}, v={}", result.r, result.s, result.v);
    result
}

/// Convert a float to wire format (string with up to 8 decimal places)
pub fn float_to_wire(x: f64) -> String {
    let rounded = format!("{:.8}", x);
    // Normalize: remove trailing zeros after decimal point
    let trimmed = rounded.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-0" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Convert a Decimal to wire format
pub fn decimal_to_wire(x: &rust_decimal::Decimal) -> String {
    // Format with 8 decimal places then normalize
    let s = format!("{:.8}", x);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-0" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Get current timestamp in milliseconds
pub fn timestamp_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_float_to_wire() {
        assert_eq!(float_to_wire(100.5), "100.5");
        assert_eq!(float_to_wire(100.0), "100");
        assert_eq!(float_to_wire(0.001), "0.001");
        assert_eq!(float_to_wire(0.00100000), "0.001");
    }

    #[test]
    fn test_address_to_bytes() {
        let addr = "0x0000000000000000000000000000000000000000";
        let bytes = address_to_bytes(addr).unwrap();
        assert_eq!(bytes, [0u8; 20]);
    }
}
