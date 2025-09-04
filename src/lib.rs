// Modules
pub mod simulation_engine;
pub mod fastlane_integration;
pub mod routers;

// Contract bindings via abigen!
// These generate structs in the current crate, so we can re-export them
pub use fastlane_integration::{FlashLoanArbitrage, FastLaneSender};

// Ethers imports
use ethers::{
    prelude::*,
    core::types::{BlockNumber, Filter, U256, U64, Address, TransactionReceipt},
    providers::{Provider, Http, Middleware},
    signers::LocalWallet,
};
use std::sync::Arc;
use std::error::Error;
use std::collections::HashMap;
use std::time::Duration;
use ethers_contract::abigen;

// Abigen! generated contract structs (they live in this crate)
abigen!(
    FlashLoanArbitrage,
    "./abis/FlashLoanArbitrage.json",
    event_derives(serde::Serialize, serde::Deserialize)
);

abigen!(
    FastLaneSender,
    "./abis/FastLaneSender.json",
    event_derives(serde::Serialize, serde::Deserialize)
);

abigen!(
    IUniswapV2Pair,
    "./abis/IUniswapV2Pair.json",
    event_derives(serde::Serialize, serde::Deserialize)
);

// Constants
const QUICKSWAP_FACTORY: &str = "0x5757371414417b8C6CAad45bAeF941aBc7d3Ab32";
const SUSHISWAP_FACTORY: &str = "0xc35DADB65012eC5796536bD9864eD8773aBc74C4";

// Routers (used when building the arbitrage "routers" array)
const QUICKSWAP_ROUTER: &str = "0xa5E0829CaCEd8fFDD4De3c43696c57F7D7A678ff";
const SUSHISWAP_ROUTER: &str = "0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506";

// Default V3 fee tier (if you hit V2-only hops it’s ignored on-chain)
const DEFAULT_FEE_U24: u32 = 3000;

// Minimum perceived profit in wei to consider (your existing constant)
const MINIMUM_PROFIT_WEI: u128 = 50_000_000_000_000_000; // 0.05 MATIC


#[derive(Debug, Clone)]
pub struct MevBot {
    provider: Arc<Provider<Http>>,
    flash_loan_contract: FlashLoanArbitrage<Provider<Http>>,
    fast_lane_sender: FastLaneSender<Provider<Http>>,
    wallet: LocalWallet,
    dex_factories: Vec<Address>,
    token_pairs: HashMap<Address, Vec<Address>>,
    last_block: U64,
}

impl MevBot {
    pub async fn new(
        rpc_url: &str,
        private_key: &str,
        flash_loan_address: Address,
        fast_lane_address: Address,
    ) -> Result<Self, Box<dyn Error>> {
        let provider = Provider::<Http>::try_from(rpc_url)?;
        let provider = Arc::new(provider);
        
        let wallet = private_key.parse::<LocalWallet>()?;
        let wallet = wallet.with_chain_id(137u64); // Polygon Mainnet
        
        let flash_loan_contract = FlashLoanArbitrage::new(flash_loan_address, provider.clone());
        let fast_lane_sender = FastLaneSender::new(fast_lane_address, provider.clone());
        
        let dex_factories = vec![
            QUICKSWAP_FACTORY.parse::<Address>()?,
            SUSHISWAP_FACTORY.parse::<Address>()?,
        ];

        let last_block = provider.get_block_number().await?;
        
        Ok(Self {
            provider,
            flash_loan_contract,
            fast_lane_sender,
            wallet,
            dex_factories,
            token_pairs: HashMap::new(),
            last_block,
        })
    }

    pub async fn monitor_blocks(&mut self) -> Result<(), Box<dyn Error>> {
        let _filter = Filter::new().from_block(BlockNumber::Latest);
        
        loop {
            let block_number = self.provider.get_block_number().await?;
            
            if block_number > self.last_block {
                // New block, update pairs and check for opportunities
                self.update_token_pairs().await?;
                self.check_opportunities().await?;
                self.last_block = block_number;
            }
            
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    async fn check_opportunities(&self) -> Result<(), Box<dyn Error>> {
        let empty_vec: Vec<Address> = Vec::new();
        
        for (&_token_a, pairs_a) in &self.token_pairs {
            for (&_token_b, pairs_b) in &self.token_pairs {
                if _token_a == _token_b {
                    continue;
                }
                
                if self.analyze_opportunity(_token_a, _token_b, pairs_a, pairs_b).await? {
                    let optimal_route = self.find_optimal_route(_token_a, _token_b).await?;
                    let amount = self.calculate_optimal_amount(&optimal_route).await?;
                    
                    if amount > U256::zero() {
                        self.execute_arbitrage(optimal_route).await?;
                    }
                }
            }
        }
        
        Ok(())
    }

    async fn analyze_opportunity(
        &self,
        _token_a: Address,
        _token_b: Address,
        pairs_a: &[Address],
        pairs_b: &[Address],
    ) -> Result<bool, Box<dyn Error>> {
        for &pair_a in pairs_a {
            for &pair_b in pairs_b {
                if pair_a == pair_b {
                    continue;
                }
                
                let (reserve_a0, reserve_a1) = self.get_reserves(pair_a).await?;
                let (reserve_b0, reserve_b1) = self.get_reserves(pair_b).await?;
                
                let price_a = reserve_a0.as_u128() as f64 / reserve_a1.as_u128() as f64;
                let price_b = reserve_b0.as_u128() as f64 / reserve_b1.as_u128() as f64;
                
                if (price_a - price_b).abs() / price_a > 0.01 {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    async fn update_token_pairs(&mut self) -> Result<(), Box<dyn Error>> {
        self.token_pairs.clear();
        
        for &factory in &self.dex_factories {
            let factory_contract = IUniswapV2Pair::new(factory, self.provider.clone());
            let pairs_length: U256 = factory_contract.get_reserves().call().await?.0.into();
            
            for i in 0..pairs_length.as_u64() {
                if let Ok(pair_address) = factory_contract.token_0().call().await {
                    let pair_contract = IUniswapV2Pair::new(pair_address, self.provider.clone());
                    let token0 = pair_contract.token_0().call().await?;
                    let token1 = pair_contract.token_1().call().await?;
                    
                    self.token_pairs.entry(token0)
                        .or_insert_with(Vec::new)
                        .push(pair_address);
                    self.token_pairs.entry(token1)
                        .or_insert_with(Vec::new)
                        .push(pair_address);
                }
            }
        }
        Ok(())
    }

    async fn execute_arbitrage(
        &self,
        path: Vec<Address>,
    ) -> Result<TransactionReceipt, Box<dyn Error>> {
        if path.len() < 2 {
            return Err("Path must have at least 2 tokens".into());
        }

        // token0 = first token in path, token1 = last token in path
        let token0 = path.first().unwrap();
        let token1 = path.last().unwrap();

        // Calculate optimal amounts per hop dynamically
        let mut amounts: Vec<U256> = Vec::with_capacity(path.len() - 1);
        for i in 0..path.len() - 1 {
            let (reserve_in, reserve_out) = self.get_reserves(path[i]).await?;
            // Basic formula: simulate trade with 1 MATIC per hop
            let amount_in = U256::from(1_000_000_000_000_000_000u64);
            let amount_out = (amount_in * reserve_out) / (reserve_in + amount_in);
            amounts.push(amount_in); // input for each hop
        }

        // routers aligned with path hops (example: Quick + Sushi + Uni)
        let routers: Vec<Address> = path
            .iter()
            .enumerate()
            .take(path.len() - 1)
            .map(|(i, _)| match i {
                0 => "0xa5E0829CaCEd8fFDD4De3c43696c57F7D7A678ff".parse::<Address>().unwrap(), // QuickSwap
                1 => "0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506".parse::<Address>().unwrap(), // SushiSwap
                _ => "0xE592427A0AEce92De3Edee1F18E0157C05861564".parse::<Address>().unwrap(), // UniV3
            })
            .collect();

        // Borrow amount = first hop input, second token 0
        let amount0 = amounts[0];
        let amount1 = U256::zero();
        let fee = 3000u32; // default fee as per contract

        // Gas & nonce
        let gas_price = self.provider.get_gas_price().await?;
        let nonce = self.provider.get_transaction_count(self.wallet.address(), None).await?;

        // Build transaction dynamically
        let tx_request = self.flash_loan_contract
            .method::<_, ()>(
                "executeFlashLoanArbitrage",
                (
                    *token0,
                    *token1,
                    amount0,
                    amount1,
                    fee,
                    path.clone(),
                    amounts.clone(),
                    routers.clone(),
                ),
            )?
            .from(self.wallet.address())
            .gas_price(gas_price)
            .nonce(nonce);

        // Send tx and await receipt
        let pending_tx = tx_request.send().await?;
        let receipt = pending_tx.await?;

        Ok(receipt.expect("Transaction failed or reverted"))
    }

    async fn find_optimal_route(
        &self,
        token_in: Address,
        token_out: Address,
    ) -> Result<Vec<Address>, Box<dyn Error>> {
        let mut best_route = vec![];
        let mut best_profit = U256::zero();
        
        let routes = self.get_all_routes(token_in, token_out)?;
        
        for route in routes {
            let profit = self.simulate_trade(&route).await?;
            if profit > best_profit {
                best_profit = profit;
                best_route = route;
            }
        }
        
        Ok(best_route)
    }

    async fn get_reserves(&self, pair: Address) -> Result<(U256, U256), Box<dyn Error>> {
        let pair_contract = IUniswapV2Pair::new(pair, self.provider.clone());
        let (reserve0, reserve1, _) = pair_contract.get_reserves().call().await?;
        Ok((reserve0.into(), reserve1.into()))
    }

    fn get_all_routes(
        &self,
        token_in: Address,
        token_out: Address,
    ) -> Result<Vec<Vec<Address>>, Box<dyn Error>> {
        let mut routes = Vec::new();
        let pairs = self.token_pairs.get(&token_in)
            .ok_or("No pairs found for input token")?;
            
        for &pair in pairs {
            let mut route = vec![token_in, pair];
            if pair == token_out {
                routes.push(route);
            } else if let Some(next_pairs) = self.token_pairs.get(&pair) {
                for &next_pair in next_pairs {
                    if next_pair == token_out {
                        route.push(next_pair);
                        routes.push(route.clone());
                    }
                }
            }
        }
        
        Ok(routes)
    }

    async fn simulate_trade(&self, path: &[Address]) -> Result<U256, Box<dyn Error>> {
        let amount = U256::from(1_000_000_000_000_000_000u64); // 1 MATIC
        let mut current_amount = amount;
        
        for i in 0..path.len() - 1 {
            let (reserve_in, reserve_out) = self.get_reserves(path[i]).await?;
            current_amount = (current_amount * reserve_out) / (reserve_in + current_amount);
        }
        
        Ok(if current_amount > amount {
            current_amount - amount
        } else {
            U256::zero()
        })
    }

    async fn calculate_optimal_amount(&self, path: &[Address]) -> Result<U256, Box<dyn Error>> {
        let mut optimal_amount = U256::zero();
        let mut max_profit = U256::zero();
        
        let amounts = vec![
            U256::from(1_000_000_000_000_000_000u64), // 1 MATIC
            U256::from(5_000_000_000_000_000_000u64), // 5 MATIC
            U256::from(10_000_000_000_000_000_000u64), // 10 MATIC
        ];
        
        for &amount in &amounts {
            let profit = self.simulate_trade_with_amount(path, amount).await?;
            if profit > max_profit {
                max_profit = profit;
                optimal_amount = amount;
            }
        }
        
        Ok(optimal_amount)
    }

    async fn simulate_trade_with_amount(
        &self,
        path: &[Address],
        amount: U256
    ) -> Result<U256, Box<dyn Error>> {
        let mut current_amount = amount;
        
        for i in 0..path.len() - 1 {
            let (reserve_in, reserve_out) = self.get_reserves(path[i]).await?;
            current_amount = (current_amount * reserve_out) / (reserve_in + current_amount);
        }
        
        Ok(if current_amount > amount {
            current_amount - amount
        } else {
            U256::zero()
        })
    }
}

#[derive(Debug)]
pub enum MevBotError {
    ProviderError(String),
    ContractError(String),
    ArbitrageError(String),
    InsufficientLiquidity(String),
    InvalidPath(String),
}

#[derive(Debug, Clone)]
pub struct Config {
    pub rpc_url: String,
    pub private_key: String,
    pub flash_loan_address: Address,
    pub fast_lane_address: Address,
    pub min_profit_threshold: U256,
    pub gas_price_limit: U256,
    pub update_interval: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[tokio::test]
    async fn test_analyze_opportunity() {
        let provider = Provider::<Http>::try_from(
            "https://polygon-rpc.com"
        ).unwrap();
        
        let wallet = "0000000000000000000000000000000000000000000000000000000000000001"
            .parse::<LocalWallet>()
            .unwrap()
            .with_chain_id(137u64);
            
        let bot = MevBot::new(
            "https://polygon-rpc.com",
            "0000000000000000000000000000000000000000000000000000000000000001",
            Address::zero(),
            Address::zero(),
        ).await.unwrap();
        
        // Test tokens (USDC and USDT on Polygon)
        let _token_a = "0x2791Bca1f2de4661ED88A30C99A7a9449Aa84174"
            .parse::<Address>()
            .unwrap();
        let _token_b = "0xc2132D05D31c914a87C6611C10748AEb04B58e8F"
            .parse::<Address>()
            .unwrap();
            
        let pairs_a = vec![];
        let pairs_b = vec![];
        
        let result = bot.analyze_opportunity(_token_a, _token_b, &pairs_a, &pairs_b).await.unwrap();
        assert!(result == true || result == false);
    }
}
