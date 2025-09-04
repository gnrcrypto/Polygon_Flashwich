// src/fastlane_integration.rs
use ethers::{
    abi::{Abi, Token, Tokenize},
    prelude::*,
    types::{
        Address, Bytes, H256, U256, U64,
    },
};
use std::sync::Arc;
use anyhow::{Result, anyhow};
use crate::simulation_engine::ArbitrageOpportunity;

// ===== Contract Bindings via Abigen =====
abigen!(
    FlashLoanArbitrage,
    "abis/FlashLoanArbitrage.json",
    event_derives(serde::Serialize, serde::Deserialize)
);

abigen!(
    FastLaneSender,
    "abis/FastLaneSender.json",
    event_derives(serde::Serialize, serde::Deserialize)
);

abigen!(
    IUniswapV2Pair,
    "abis/IUniswapV2Pair.json",
    event_derives(serde::Serialize, serde::Deserialize)
);

// ===== Structs for ABI Encoding =====
#[derive(Clone)]
pub struct UserOp {
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub gas: U256,
    pub max_fee_per_gas: U256,
    pub nonce: U256,
    pub deadline: U256,
    pub dapp: Address,
    pub control: Address,
    pub call_config: u32,
    pub session_key: Address,
    pub data: Bytes,
    pub signature: Bytes,
}

#[derive(Clone)]
pub struct SolverOp {
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub gas: U256,
    pub max_fee_per_gas: U256,
    pub deadline: U256,
    pub solver: Address,
    pub control: Address,
    pub user_op_hash: [u8; 32],
    pub data: Bytes,
    pub signature: Bytes,
}

#[derive(Clone)]
pub struct DAppOp {
    pub from: Address,
    pub to: Address,
    pub value: U256,
    pub gas: U256,
    pub max_fee_per_gas: U256,
    pub deadline: U256,
    pub dapp: Address,
    pub control: Address,
    pub data: Bytes,
    pub signature: Bytes,
}

#[derive(Clone, Debug)]
pub struct FastLaneBundle {
    pub data: Bytes,
    pub target_block: U64,
}

#[derive(Clone, Copy, Debug)]
pub enum BundleStatus {
    Unknown,
    Pending,
    Included,
    Replaced,
}

// ===== Tokenize Trait Implementations =====
impl Tokenize for UserOp {
    fn into_tokens(self) -> Vec<Token> {
        vec![
            Token::Address(self.from),
            Token::Address(self.to),
            Token::Uint(self.value),
            Token::Uint(self.gas),
            Token::Uint(self.max_fee_per_gas),
            Token::Uint(self.nonce),
            Token::Uint(self.deadline),
            Token::Address(self.dapp),
            Token::Address(self.control),
            Token::Uint(self.call_config.into()),
            Token::Address(self.session_key),
            Token::Bytes(self.data.to_vec()),
            Token::Bytes(self.signature.to_vec()),
        ]
    }
}

impl Tokenize for SolverOp {
    fn into_tokens(self) -> Vec<Token> {
        vec![
            Token::Address(self.from),
            Token::Address(self.to),
            Token::Uint(self.value),
            Token::Uint(self.gas),
            Token::Uint(self.max_fee_per_gas),
            Token::Uint(self.deadline),
            Token::Address(self.solver),
            Token::Address(self.control),
            Token::FixedBytes(self.user_op_hash.to_vec()),
            Token::Bytes(self.data.to_vec()),
            Token::Bytes(self.signature.to_vec()),
        ]
    }
}

impl Tokenize for DAppOp {
    fn into_tokens(self) -> Vec<Token> {
        vec![
            Token::Address(self.from),
            Token::Address(self.to),
            Token::Uint(self.value),
            Token::Uint(self.gas),
            Token::Uint(self.max_fee_per_gas),
            Token::Uint(self.deadline),
            Token::Address(self.dapp),
            Token::Address(self.control),
            Token::Bytes(self.data.to_vec()),
            Token::Bytes(self.signature.to_vec()),
        ]
    }
}

// ===== FastLane Client =====
pub struct FastLaneClient {
    provider: Arc<Provider<Ws>>,
    wallet: LocalWallet,
    fastlane_address: Address,
    fastlane_sender_contract: Address,
    solver_contract: Address,
    max_delay_blocks: U256,
    min_priority_fee: U256,
}

impl FastLaneClient {
    pub fn new(
        provider: Arc<Provider<Ws>>,
        wallet: LocalWallet,
        fastlane_address: Address,
        fastlane_sender_contract: Address,
        solver_contract: Address,
        max_delay_blocks: U256,
        min_priority_fee: U256,
    ) -> Self {
        Self {
            provider,
            wallet,
            fastlane_address,
            fastlane_sender_contract,
            solver_contract,
            max_delay_blocks,
            min_priority_fee,
        }
    }

    fn load_abi(bytes: &[u8]) -> Result<Abi> {
        let abi: Abi = serde_json::from_slice(bytes)?;
        Ok(abi)
    }

    pub async fn create_fastlane_bundle(
        &self,
        opportunity: &ArbitrageOpportunity,
        target_block: U64,
    ) -> Result<FastLaneBundle> {
        let abi = Self::load_abi(include_bytes!("../abis/FlashLoanArbitrage.json"))?;
        let contract = Contract::new(self.solver_contract, abi, self.provider.clone());

        let calldata = contract
            .method::<_, Bytes>("executeFlashLoanArbitrage", (opportunity.clone(),))?
            .calldata()
            .ok_or(anyhow!("Failed to generate calldata"))?;

        Ok(FastLaneBundle {
            data: calldata,
            target_block,
        })
    }

    pub async fn submit_raw_transaction(
        &self,
        bundle: &FastLaneBundle,
        gas_price: U256,
    ) -> Result<H256> {
        let fastlane_sender_abi = Self::load_abi(include_bytes!("../abis/FastLaneSender.json"))?;
        let fastlane_sender_contract = Contract::new(
            self.fastlane_sender_contract,
            fastlane_sender_abi,
            self.provider.clone()
        );

        let tx = fastlane_sender_contract
            .method::<_, H256>(
                "sendRawTransaction",
                (bundle.data.clone(), bundle.target_block.as_u64())
            )?
            .gas_price(gas_price)
            .from(self.wallet.address());

        let pending_tx = tx.send().await?;
        let receipt = pending_tx.await?;

        receipt.map_or(
            Err(anyhow!("Transaction receipt not found")),
            |r| Ok(r.transaction_hash)
        )
    }

    pub fn validate_bundle_params(&self, target_block: U64, current_block: U64) -> Result<()> {
        if target_block <= current_block {
            return Err(anyhow!("Target block must be in the future"));
        }
        if target_block > current_block + 5 {
            return Err(anyhow!("Target block too far in the future"));
        }
        Ok(())
    }
}

// ===== Re-export generated structs for external use =====
pub use FlashLoanArbitrage;

