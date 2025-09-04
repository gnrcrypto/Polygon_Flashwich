// src/main.rs
mod simulation_engine;
mod fastlane_integration;
pub mod routers;

use anyhow::{Result, bail};
use ethers::{
    middleware::Middleware,
    providers::{Provider, StreamExt, Ws},
    types::{Address, U256, BlockNumber, U64, TransactionReceipt},
    signers::{LocalWallet, Signer},
    contract::abigen,
};
use log::{info, warn, debug, error};
use std::str::FromStr;
use std::sync::Arc;
use std::collections::HashMap;
use std::convert::From;

// Import token data
use serde_json::Value;
use std::fs;

// Simulation and routing modules
use simulation_engine::{
    ArbitrageOpportunity,
    AdvancedSimulationEngine,
};
use fastlane_integration::FastLaneClient;
use routers::{
    quickswap::QuickswapRouter,
    uniswap_v3::UniswapV3Router,
    sushiswap::SushiswapRouter,
};

// Define the contract ABI for the Flash Loan contract
abigen!(FlashLoanContract, "abis/FlashLoanArbitrage.json",);

// Constants for common tokens on Polygon
const WETH: &str = "0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270"; // WMATIC
const USDC: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
const USDT: &str = "0xc2132D05D31c914a87C6611C10748AEb04B58e8F";

// Flash Loan Arbitrage Struct
struct FlashLoanArbitrage {
    provider: Arc<Provider<Ws>>,
    engine: AdvancedSimulationEngine,
    fastlane_client: FastLaneClient,
    flash_loan_contract: Address,
    wallet: LocalWallet,
    tokens: HashMap<String, Value>,
}

impl FlashLoanArbitrage {
    fn new(
        provider: Arc<Provider<Ws>>,
        flash_loan_contract: Address,
        fastlane_address: Address,
        fastlane_sender_address: Address,
        solver_address: Address,
        wallet: LocalWallet,
        max_delay_blocks: U256,
        min_priority_fee: U256,
    ) -> Result<Self> {
        // Load tokens from JSON
        let tokens_path = "./src/tokens.json";
        let tokens_content = fs::read_to_string(tokens_path)?;
        let tokens: HashMap<String, Value> = serde_json::from_str(&tokens_content)?;

        // Initialize routers
        let quickswap_router = QuickswapRouter::new(provider.clone());
        let sushiswap_router = SushiswapRouter::new(provider.clone());
        let uniswap_v3_router = UniswapV3Router::new(provider.clone());

        let engine = AdvancedSimulationEngine::new(
            provider.clone(),
            quickswap_router,
            sushiswap_router,
            uniswap_v3_router,
        );

        let fastlane_client = FastLaneClient::new(
            provider.clone(),
            wallet.clone(),
            fastlane_address,
            fastlane_sender_address,
            solver_address,
            max_delay_blocks,
            min_priority_fee,
        );

        Ok(Self {
            provider,
            engine,
            fastlane_client,
            flash_loan_contract,
            wallet,
            tokens,
        })
    }


    // Enhanced multi-leg arbitrage method
    async fn execute_multi_leg_arbitrage(
        &self,
        opportunity: &ArbitrageOpportunity
    ) -> Result<TransactionReceipt> {
        // Validate arbitrage route
        if opportunity.routers.is_empty() {
            bail!("No arbitrage routes found");
        }

        // Get current block for targeting
        let current_block = self.provider.get_block(BlockNumber::Latest)
            .await?
            .ok_or_else(|| anyhow::anyhow!("Could not fetch current block"))?
            .number
            .ok_or_else(|| anyhow::anyhow!("Block number not available"))?;

        let target_block = U64::from(current_block.as_u64() + 1);

        // Create FastLane bundle
        let _bundle = self.fastlane_client
            .create_fastlane_bundle(opportunity, target_block)
            .await?;

        // Corrected method call - using the proper function signature from ABI
        let contract = FlashLoanContract::new(self.flash_loan_contract, Arc::clone(&self.provider));
        
        // Create the ArbitrageOpportunity struct expected by the contract
        let arbitrage_opportunity = FlashLoanContractArbitrageOpportunity {
            token0: opportunity.token0,
            token1: opportunity.token1,
            amount0: opportunity.amount0,
            amount1: opportunity.amount1,
            fee: opportunity.fee.unwrap_or(3000), // Default fee if not specified
            path: opportunity.path.clone(),
            amounts: opportunity.amounts.clone(),
            routers: opportunity.routers.clone(),
        };

        let tx = contract.execute_arbitrage_with_fast_lane(
            arbitrage_opportunity,
            target_block
        )
        .value(opportunity.expected_profit.unwrap_or(U256::zero())) // Add value for FastLane bid
        .send()
        .await?
        .await?
        .ok_or_else(|| anyhow::anyhow!("No receipt returned"))?;

        Ok(tx)
    }

    // Mempool monitoring method
    async fn start_monitoring(&self) -> Result<()> {
        let mut stream = self.provider.subscribe_pending_txs().await?;

        info!("Mempool monitor started. Listening for pending transactions...");

        while let Some(tx_hash) = stream.next().await {
            debug!("Received new pending tx: {:?}", tx_hash);

            // Fetch the full transaction object from the hash
            let tx_result = self.provider.get_transaction(tx_hash).await;

            // Check if the transaction was found
            let tx = match tx_result {
                Ok(Some(t)) => t,
                Ok(None) => {
                    debug!("Transaction with hash {:?} not found in mempool.", tx_hash);
                    continue;
                },
                Err(e) => {
                    error!("Error fetching transaction {:?}: {:?}", tx_hash, e);
                    continue;
                }
            };

            // Simulate potential arbitrage
            match self.engine.simulate_arbitrage_opportunity(&tx).await {
                Ok(Some(opportunity)) => {
                    info!("Profitable arbitrage found! Profit: {:?}", opportunity.expected_profit);

                    // Execute multi-leg arbitrage
                    match self.execute_multi_leg_arbitrage(&opportunity).await {
                        Ok(receipt) => {
                            info!("Arbitrage executed successfully. Tx Hash: {:?}", receipt.transaction_hash);
                        }
                        Err(e) => {
                            warn!("Arbitrage execution failed: {:?}", e);
                        }
                    }
                }
                Ok(None) => {
                    debug!("No profitable arbitrage opportunity found.");
                }
                Err(e) => {
                    error!("Arbitrage simulation error: {:?}", e);
                }
            }
        }

        Ok(())
    }
}


#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging and environment variables
    env_logger::init();
    dotenv::dotenv().ok();

    // WebSocket provider setup
    let ws_url = std::env::var("POLYGON_WS_URL")
        .expect("POLYGON_WS_URL must be set in .env");
    let provider = Provider::connect(&ws_url).await?;
    let provider = Arc::new(provider);

    // Contract addresses from environment
    let flash_loan_contract = Address::from_str(
        &std::env::var("FLASH_LOAN_CONTRACT")
            .expect("FLASH_LOAN_CONTRACT must be set in .env")
    )?;

    let fastlane_address = Address::from_str(
        &std::env::var("FASTLANE_CONTRACT")
            .expect("FASTLANE_CONTRACT must be set in .env")
    )?;

    let fastlane_sender_address = Address::from_str(
        &std::env::var("FASTLANE_SENDER_CONTRACT")
            .expect("FASTLANE_SENDER_CONTRACT must be set in .env")
    )?;

    let solver_address = Address::from_str(
        &std::env::var("ARBITRAGE_EXECUTOR_CONTRACT")
            .expect("ARBITRAGE_EXECUTOR_CONTRACT must be set in .env")
    )?;

    let solver_contract = ISolverContract::new(
        config.solver_contract_address,
        Arc::new(provider.clone()),
    );
    
    let fastlane_contract = FastLaneContract::new(
        config.fastlane_contract_address,
        Arc::new(provider.clone()),
    );
    
    let pfl_dapp_contract = PFLDAppContract::new(
        config.pfl_dapp_address,
        Arc::new(provider.clone()),
    );
    
    let dapp_signer_contract = DAppSignerContract::new(
        config.dapp_signer_address,
        Arc::new(provider.clone()),
    );

    // Wallet setup
    let private_key = std::env::var("WALLET_PRIVATE_KEY")
        .expect("WALLET_PRIVATE_KEY must be set in .env");
    let wallet: LocalWallet = private_key.parse()?;

    // Configuration parameters
    let max_delay_blocks = U256::from(3);
    let min_priority_fee = U256::from(1_000_000_000u64); // 1 gwei

    // Initialize arbitrage bot
    let arbitrage_bot = FlashLoanArbitrage::new(
        provider.clone(),
        flash_loan_contract,
        fastlane_address,
        fastlane_sender_address,
        solver_address,
        wallet.clone(),
        max_delay_blocks,
        min_priority_fee,
    )?;

    // Start monitoring in a separate task
    let bot_clone = Arc::new(arbitrage_bot);
    let _monitoring_task = {
        let bot = bot_clone.clone();
        tokio::spawn(async move {
            if let Err(e) = bot.start_monitoring().await {
                error!("Monitoring failed: {:?}", e);
            }
        })
    };

    info!("Polygon Flash Arbitrage Bot initialized. Press CTRL+C to exit.");

    // Wait for termination signal
    tokio::signal::ctrl_c().await?;

    Ok(())
}
