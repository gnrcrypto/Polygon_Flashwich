// src/fastlane_integration.rs
use anyhow::{Result, bail};
use ethers::{
    providers::Middleware,
    types::{Address, U256, U64, Bytes, TransactionRequest},
    contract::abigen,
    signers::LocalWallet,
};
use log::{info, warn, debug, error};
use std::sync::Arc;

use crate::ArbitrageOpportunity;

// Define the FastLane contract ABI
abigen!(FastLaneContract, "abis/FastLane.json",);

// FastLane client for submitting bundles
pub struct FastLaneClient {
    provider: Arc<dyn Middleware>,
    wallet: LocalWallet,
    fastlane_contract: Address,
    fastlane_sender: Address,
}

impl FastLaneClient {
    pub fn new(
        provider: Arc<dyn Middleware>,
        wallet: LocalWallet,
        fastlane_contract: Address,
        fastlane_sender: Address,
    ) -> Self {
        Self {
            provider,
            wallet,
            fastlane_contract,
            fastlane_sender,
        }
    }

    // Enable EOA for FastLane
    pub async fn enable_eoa(&self) -> Result<()> {
        let contract = FastLaneContract::new(self.fastlane_contract, Arc::clone(&self.provider));
        
        let tx = contract.enable_eoa()
            .send()
            .await?
            .await?;

        info!("EOA enabled for FastLane: {:?}", tx);
        Ok(())
    }

    // Create and submit FastLane bundle
    pub async fn create_fastlane_bundle(
        &self,
        opportunity: &ArbitrageOpportunity,
        target_block: U64,
    ) -> Result<Bytes> {
        // Encode the arbitrage opportunity data
        let opportunity_data = abi::encode(&[
            abi::Token::Address(opportunity.token0),
            abi::Token::Address(opportunity.token1),
            abi::Token::Uint(opportunity.amount0),
            abi::Token::Uint(opportunity.amount1),
            abi::Token::Uint(U256::from(opportunity.fee.unwrap_or(3000))),
            abi::Token::Array(opportunity.path.iter().map(|&addr| abi::Token::Address(addr)).collect()),
            abi::Token::Array(opportunity.amounts.iter().map(|&amt| abi::Token::Uint(amt)).collect()),
            abi::Token::Array(opportunity.routers.iter().map(|&addr| abi::Token::Address(addr)).collect()),
        ]);

        // Create the solver transaction (our arbitrage contract)
        let solver_tx = TransactionRequest::new()
            .to(opportunity.solver_contract.unwrap_or(Address::zero()))
            .data(opportunity_data)
            .gas(U256::from(5000000));

        // Create the opportunity transaction (the transaction we're frontrunning)
        let opportunity_tx = TransactionRequest::new()
            .to(opportunity.target_contract)
            .data(opportunity.calldata.clone())
            .gas(U256::from(5000000));

        // Encode both transactions into bundle data
        let bundle_data = abi::encode(&[
            abi::Token::Bytes(opportunity_tx.data.unwrap_or_default().to_vec()),
            abi::Token::Bytes(solver_tx.data.unwrap_or_default().to_vec()),
        ]);

        // Submit to FastLane
        let contract = FastLaneContract::new(self.fastlane_contract, Arc::clone(&self.provider));
        
        let bid_amount = opportunity.expected_profit.unwrap_or(U256::zero()) * 8 / 10; // 80% of expected profit
        let min_bid = U256::from(1_000_000_000_000_000); // 0.001 MATIC minimum
        let actual_bid = if bid_amount < min_bid { min_bid } else { bid_amount };

        let tx = contract.submit_bundle(
            Bytes::from(bundle_data),
            target_block,
            actual_bid
        )
        .value(actual_bid)
        .send()
        .await?
        .await?;

        info!("FastLane bundle submitted for block {} with bid {} wei", target_block, actual_bid);
        
        Ok(Bytes::from(bundle_data))
    }

    // Check bundle status
    pub async fn check_bundle_status(&self, bundle_hash: Bytes) -> Result<bool> {
        let contract = FastLaneContract::new(self.fastlane_contract, Arc::clone(&self.provider));
        
        let status = contract.get_bundle_status(bundle_hash).call().await?;
        
        Ok(status)
    }
}
