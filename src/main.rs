// src/main.rs
mod simulation_engine;
mod fastlane_integration;
pub mod routers;

use ethers::middleware::Middleware;
use routers::{
    quickswap::QuickswapRouter,
    uniswap_v3::UniswapV3Router,
    sushiswap::SushiswapRouter,
};

use anyhow::Result;
use ethers::{
    providers::{Provider, StreamExt, Ws},
    types::{Address, U256, BlockNumber, U64},
    signers::LocalWallet,
};
use log::{info, warn, debug};
use std::str::FromStr;
use std::sync::Arc;
use simulation_engine::{ArbitrageOpportunity, AdvancedSimulationEngine};
use fastlane_integration::FastLaneClient;
use dotenv::dotenv;
use std::env;

// Constants for common tokens on Polygon
const WETH: &str = "0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270"; // WMATIC
const USDC: &str = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174";
const USDT: &str = "0xc2132D05D31c914a87C6611C10748AEb04B58e8F";

struct MempoolMonitor {
    provider: Arc<Provider<Ws>>,
    engine: AdvancedSimulationEngine,
    fastlane_client: FastLaneClient,
    flash_loan_contract: Address,
    wallet: LocalWallet,
}

impl MempoolMonitor {
    fn new(
        provider: Arc<Provider<Ws>>,
        flash_loan_contract: Address,
        fastlane_address: Address,
        fastlane_sender_address: Address,
        solver_address: Address,
        wallet: LocalWallet,
        max_delay_blocks: U256,
        min_priority_fee: U256,
    ) -> Self {
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

        Self {
            provider,
            engine,
            fastlane_client,
            flash_loan_contract,
            wallet,
        }
    }

    async fn start_monitoring(&self) -> Result<()> {
        let mut stream = self.provider.subscribe_pending_txs().await?;

        info!("Mempool monitor started. Listening for pending transactions...");

        while let Some(tx_hash) = stream.next().await {
            debug!("Received new transaction hash: {:?}", tx_hash);

            let tx = match self.provider.get_transaction(tx_hash).await {
                Ok(Some(tx)) => tx,
                _ => continue,
            };

            if let Some(to) = tx.to {
                if to == self.flash_loan_contract {
                    info!("Potential arbitrage opportunity detected in transaction: {:?}", tx_hash);

                    match self.engine.simulate_arbitrage_opportunity(&tx).await {
                        Ok(Some(sim_result)) => {
                            info!("Simulation successful. Expected profit: {:?}", sim_result.expected_profit);

                            if sim_result.expected_profit > U256::zero() {
                                info!("Found a profitable arbitrage opportunity! Sending to FastLane...");

                                // ✅ Use sim_result directly, no re-mapping
                                let opportunity: ArbitrageOpportunity = sim_result;

                                let current_block = self.provider.get_block(BlockNumber::Latest)
                                    .await?.unwrap().number.unwrap();
                                let target_block = U64::from(current_block.as_u64() + 1);

                                let bundle = self.fastlane_client
                                    .create_fastlane_bundle(&opportunity, target_block)
                                    .await?;
                                info!("Bundle created: {:?}", bundle);
                                info!("Bundle prepared and ready to send. Target block: {:?}", target_block);
                            }
                        }
                        Ok(None) => {
                            debug!("Simulation did not find a profitable opportunity.");
                        }
                        Err(e) => {
                            warn!("Simulation failed: {:?}", e);
                        }
                    }
                }
            }
        }

        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    dotenv().ok();

    let ws_url = env::var("POLYGON_WS_URL")
        .expect("POLYGON_WS_URL must be set in .env");

    let provider = Provider::<Ws>::connect(&ws_url).await?;
    let provider = Arc::new(provider);

    let flash_loan_contract = Address::from_str(
        &env::var("FLASH_LOAN_CONTRACT").expect("FLASH_LOAN_CONTRACT must be set in .env")
    )?;

    let fastlane_address = Address::from_str(
        &env::var("FASTLANE_CONTRACT").expect("FASTLANE_CONTRACT must be set in .env")
    )?;

    let fastlane_sender_address = Address::from_str(
        &env::var("FASTLANE_SENDER_CONTRACT").expect("FASTLANE_SENDER_CONTRACT must be set in .env")
    )?;

    let solver_address = Address::from_str(
        &env::var("ARBITRAGE_EXECUTOR_CONTRACT").expect("ARBITRAGE_EXECUTOR_CONTRACT must be set in .env")
    )?;

    let private_key = env::var("WALLET_PRIVATE_KEY").expect("WALLET_PRIVATE_KEY must be set in .env");
    let wallet: LocalWallet = private_key.parse::<LocalWallet>()?;

    // Config params
    let max_delay_blocks = U256::from(3);
    let min_priority_fee = U256::from(1_000_000_000u64); // 1 gwei

    let monitor = Arc::new(MempoolMonitor::new(
        provider.clone(),
        flash_loan_contract,
        fastlane_address,
        fastlane_sender_address,
        solver_address,
        wallet.clone(),
        max_delay_blocks,
        min_priority_fee,
    ));

    let monitor_clone = monitor.clone();
    tokio::spawn(async move {
        if let Err(e) = monitor_clone.start_monitoring().await {
            warn!("Mempool monitoring error: {:?}", e);
        }
    });

    info!("Bot is running. Press CTRL+C to exit.");

    tokio::signal::ctrl_c().await?;

    Ok(())
}
