use ethers::{
    abi::Abi,
    prelude::*,
    types::{Address, Bytes, U256},
};
use std::sync::Arc;
use anyhow::Result;
use serde_json;

pub const UNISWAP_V3_ROUTER: &str = "0xE592427A0AEce92De3Edee1F18E0157C05861564";
pub const UNISWAP_V3_FACTORY: &str = "0x1F98431c8aD98523631AE4a59f267346ea31F984";
pub const DEFAULT_FEE: u32 = 3000; // 0.3%
pub const FEE_TIERS: [u32; 3] = [500, 3000, 10000];

#[derive(Debug, Clone)]
pub struct UniswapV3Router {
    pub address: Address,
    provider: Arc<Provider<Ws>>,
}

impl UniswapV3Router {
    pub fn new(provider: Arc<Provider<Ws>>) -> Self {
        Self {
            address: UNISWAP_V3_ROUTER.parse().unwrap(),
            provider,
        }
    }

    // Helper function to load ABI properly
    fn load_uniswap_v3_abi() -> Result<Abi> {
        let abi_bytes = include_bytes!("../../abis/UniswapV3Router.json");
        let abi: Abi = serde_json::from_slice(abi_bytes)?;
        Ok(abi)
    }

    pub async fn exact_input_single(
        &self,
        params: ExactInputSingleParams,
    ) -> Result<Bytes> {
        let abi = Self::load_uniswap_v3_abi()?;
        let contract = Contract::new(
            self.address,
            abi,
            self.provider.clone(),
        );

        // Instead of passing the struct directly, pass individual parameters
        // This avoids the Tokenizable trait requirement
        Ok(contract
            .method::<_, Bytes>(
                "exactInputSingle",
                (
                    params.token_in,
                    params.token_out,
                    params.fee,
                    params.recipient,
                    params.deadline,
                    params.amount_in,
                    params.amount_out_minimum,
                    params.sqrt_price_limit_x96,
                ),
            )?
            .calldata()
            .unwrap())
    }

    // Alternative method that takes individual parameters
    pub async fn exact_input_single_params(
        &self,
        token_in: Address,
        token_out: Address,
        fee: u32,
        recipient: Address,
        deadline: U256,
        amount_in: U256,
        amount_out_minimum: U256,
        sqrt_price_limit_x96: U256,
    ) -> Result<Bytes> {
        let abi = Self::load_uniswap_v3_abi()?;
        let contract = Contract::new(
            self.address,
            abi,
            self.provider.clone(),
        );

        Ok(contract
            .method::<_, Bytes>(
                "exactInputSingle",
                (
                    token_in,
                    token_out,
                    fee,
                    recipient,
                    deadline,
                    amount_in,
                    amount_out_minimum,
                    sqrt_price_limit_x96,
                ),
            )?
            .calldata()
            .unwrap())
    }
}

#[derive(Debug, Clone)]
pub struct ExactInputSingleParams {
    pub token_in: Address,
    pub token_out: Address,
    pub fee: u32,
    pub recipient: Address,
    pub deadline: U256,
    pub amount_in: U256,
    pub amount_out_minimum: U256,
    pub sqrt_price_limit_x96: U256,
}
