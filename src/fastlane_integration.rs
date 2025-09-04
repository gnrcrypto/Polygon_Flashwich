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
