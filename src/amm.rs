use std::time::Duration;

// Performs interactions with AMMs (Automated Market Makers) on ethereum
use crate::{client::Web3, jsonrpc::error::Web3Error, types::SendTxOption};
use clarity::utils::display_uint256_as_address;
use clarity::{
    abi::{encode_call, Token},
    constants::{TT160M1, TT24M1},
    Address, PrivateKey, Uint256,
};
use num::traits::Inv;
use num::BigUint;
use tokio::time::timeout as future_timeout;

/// Default padding multiplied to uniswap exchange gas limit values due to variablity of gas limit values
/// between iterations
pub const DEFAULT_GAS_LIMIT_MULT: f32 = 1.2;

lazy_static! {
    /// Uniswap V3's Quoter interface for checking current swap prices, from prod Ethereum
    pub static ref UNISWAP_QUOTER_ADDRESS: Address =
        Address::parse_and_validate("0xb27308f9F90D607463bb33eA1BeBb41C27CE5AB6").unwrap();
    /// Uniswap V3's Router interface for swapping tokens, from prod Ethereum
    pub static ref UNISWAP_ROUTER_ADDRESS: Address =
        Address::parse_and_validate("0xE592427A0AEce92De3Edee1F18E0157C05861564").unwrap();
    pub static ref UNISWAP_FACTORY_ADDRESS: Address =
        Address::parse_and_validate("0x1F98431c8aD98523631AE4a59f267346ea31F984").unwrap();
    /// The DAI V2 Token's address, on prod Ethereum
    pub static ref DAI_CONTRACT_ADDRESS: Address =
        Address::parse_and_validate("0x6B175474E89094C44Da98b954EedeAC495271d0F").unwrap();
    /// The Wrapped Ether's address, on prod Ethereum
    pub static ref WETH_CONTRACT_ADDRESS: Address =
        Address::parse_and_validate("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2").unwrap();
}

impl Web3 {
    /// Checks Uniswap v3 to get the amount of `token_out` obtainable for `amount` of `token_in`
    /// Returns an error if the pool's liquidity is too low, resulting in a swap returning less than what the
    /// sqrt_price_limit_x96_uint160 implies should be traded
    ///
    /// # Arguments
    ///
    /// * `caller_address` - The ethereum address making the request
    /// * `token_in` - The address of an ERC20 token to offer up
    /// * `token_out` - The address of an ERC20 token to receive
    /// * `fee_uint24` - Optional fee level of the `token_in`<->`token_out` pool to query - limited to uint24 in size.
    ///    Defaults to the pool fee of 0.3%
    ///    The suggested pools are 0.3% (3000), 0.05% (500), 1% (10000), and 0.01% (100) but more may be added permissionlessly
    /// * `sqrt_price_limit_x96_uint160` - Optional square root price limit, see methods below for more information
    /// * `uniswap_quoter` - Optional address of the Uniswap v3 quoter to contact
    ///
    /// # Examples
    /// ```rust,ignore
    /// use std::time::Duration;
    /// use std::str::FromStr;
    /// use clarity::Address;
    /// use clarity::Uint256;
    /// use web30::amm::*;
    /// use web30::client::Web3;
    /// let web3 = Web3::new("https://eth.althea.net", Duration::from_secs(5));
    /// let result = web3.get_uniswap_price(
    ///     Address::parse_and_validate("0x1111111111111111111111111111111111111111").unwrap(),
    ///     *WETH_CONTRACT_ADDRESS,
    ///     *DAI_CONTRACT_ADDRESS,
    ///     Some(500u16.into()),
    ///     Uint256::from_str("1000000000000000000"), // 1 WETH
    ///     Some(uniswap_sqrt_price(2023u16.into(), 1u8.into())), // Sample 1 Eth ->  2k Dai swap rate,
    ///     Some(*UNISWAP_QUOTER_ADDRESS),
    /// );
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub async fn get_uniswap_price(
        &self,
        caller_address: Address,
        token_in: Address,                             // The token held
        token_out: Address,                            // The desired token
        fee_uint24: Option<Uint256>,                   // Actually a uint24 on the callee side
        amount: Uint256,                               // The amount of tokens offered up
        sqrt_price_limit_x96_uint160: Option<Uint256>, // Actually a uint160 on the callee side
        uniswap_quoter: Option<Address>, // The default quoter will be used if none is provided
    ) -> Result<Uint256, Web3Error> {
        let quoter = uniswap_quoter.unwrap_or(*UNISWAP_QUOTER_ADDRESS);

        let fee_uint24 = fee_uint24.unwrap_or_else(|| 3000u32.into());
        if bad_fee(&fee_uint24) {
            return Err(Web3Error::BadInput(
                "Bad fee input to swap price - value too large for uint24".to_string(),
            ));
        }

        let sqrt_price_limit_x96 = sqrt_price_limit_x96_uint160.clone().unwrap_or_default();
        if bad_sqrt_price_limit(&sqrt_price_limit_x96) {
            return Err(Web3Error::BadInput(
                "Bad sqrt_price_limit_x96 input to swap price - value too large for uint160"
                    .to_string(),
            ));
        }

        let tokens: [Token; 5] = [
            Token::Address(token_in),
            Token::Address(token_out),
            Token::Uint(fee_uint24.clone()),
            Token::Uint(amount.clone()),
            Token::Uint(sqrt_price_limit_x96.clone()),
        ];

        debug!("tokens is  {:?}", tokens);
        let payload = encode_call(
            "quoteExactInputSingle(address,address,uint24,uint256,uint160)",
            &tokens,
        )?;
        let result = self
            .simulate_transaction(quoter, 0u8.into(), payload, caller_address, None)
            .await?;
        debug!("result is {:?}", result);

        // Compute a sensible minimum amount out to determine if too little liquidity exists for the swap
        let amount_out_min: Uint256 = self
            .get_sensible_amount_out_from_sqrt_price(
                sqrt_price_limit_x96_uint160,
                amount.clone(),
                token_in,
                token_out,
                fee_uint24.clone(),
                caller_address,
            )
            .await?;

        let decoded_sqrt_price = decode_uniswap_sqrt_price(sqrt_price_limit_x96.clone());

        let amount_out = Uint256::from_bytes_be(match result.get(0..32) {
            Some(val) => val,
            None => {
                return Err(Web3Error::ContractCallError(
                    "Bad response from swap price".to_string(),
                ))
            }
        });

        if amount_out < amount_out_min {
            let amount_in_pretty = amount.to_string().parse::<f64>().unwrap() / 10f64.powi(18);
            let acceptable_amount_pretty =
                amount_out_min.to_string().parse::<f64>().unwrap() / 10f64.powi(18);
            let actual_amount_pretty =
                amount_out.to_string().parse::<f64>().unwrap() / 10f64.powi(18);
            warn!(
                "Attempted to get swap amount for {} {} with sqrt price {}, expected at least {} but swap was for {}",
                amount_in_pretty, token_in, decoded_sqrt_price, acceptable_amount_pretty, actual_amount_pretty,
            );
            return Err(Web3Error::BadResponse("Liquidity too low".to_string()));
        }

        Ok(amount_out)
    }

    /// Performs an exact input single pool swap via Uniswap v3, exchanging `amount` of `token_in` for `token_out`
    ///
    /// # Arguments
    /// * `eth_private_key` - The private key of the holder of `token_in` who will receive `token_out`
    /// * `token_in` - The address of the ERC20 token to exchange for `token_out`
    /// * `token_out` - The address of the ERC20 token to receive
    /// * `fee_uint24` - Optional fee level of the `token_in`<->`token_out` pool to query - limited to uint24 in size.
    ///    Defaults to the medium pool fee of 0.3%
    ///    The suggested pools are 0.3% (3000), 0.05% (500), 1% (10000), and 0.01% (100) but more may be added permissionlessly
    /// * `amount` - The amount of `token_in` to exchange for as much `token_out` as possible
    /// * `deadline` - Optional deadline to the swap before it is cancelled, 10 minutes if None
    /// * `amount_out_min` - Optional minimum amount of `token_out` to receive or the swap is cancelled, ignored if None
    /// * `sqrt_price_limit_x96_64` - Optional square root price limit, ignored if None or 0.
    ///                               See the methods below for more information
    /// * `uniswap_router` - Optional address of the Uniswap v3 SwapRouter to contact
    /// * `options` - Optional arguments for the Transaction, see send_transaction()
    /// * `wait_timeout` - Set to Some(TIMEOUT) if you wish to wait for this tx to enter the chain before returning
    ///
    /// # Examples
    /// ```
    /// use std::time::Duration;
    /// use clarity::PrivateKey;
    /// use web30::amm::*;
    /// use web30::client::Web3;
    /// let web3 = Web3::new("http://localhost:8545", Duration::from_secs(5));
    /// let result = web3.swap_uniswap(
    ///     "0x1111111111111111111111111111111111111111111111111111111111111111".parse().unwrap(),
    ///     *WETH_CONTRACT_ADDRESS,
    ///     *DAI_CONTRACT_ADDRESS,
    ///     Some(500u16.into()),
    ///     1000000000000000000u128.into(), // 1 WETH
    ///     Some(60u8.into()), // Wait 1 minute
    ///     Some(2020000000000000000000u128.into()), // Expect >= 2020 DAI
    ///     Some(uniswap_sqrt_price_from_amounts(1u8.into(), 2000u16.into())), // Sample 1 Eth ->  2k Dai swap rate
    ///     Some(*UNISWAP_ROUTER_ADDRESS),
    ///     None,
    ///     None,
    /// );
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub async fn swap_uniswap(
        &self,
        eth_private_key: PrivateKey,     // The address swapping tokens
        token_in: Address,               // The token held
        token_out: Address,              // The desired token
        fee_uint24: Option<Uint256>,     // Actually a uint24 on the callee side
        amount: Uint256,                 // The amount of tokens offered up
        deadline: Option<Uint256>,       // A deadline by which the swap must happen
        amount_out_min: Option<Uint256>, // The minimum output tokens to receive in a swap
        sqrt_price_limit_x96_uint160: Option<Uint256>, // Actually a uint160 on the callee side
        uniswap_router: Option<Address>, // The default router will be used if None is provided
        options: Option<Vec<SendTxOption>>, // Options for send_transaction
        wait_timeout: Option<Duration>,
    ) -> Result<Uint256, Web3Error> {
        let fee_uint24 = fee_uint24.unwrap_or_else(|| 3000u16.into());
        if bad_fee(&fee_uint24) {
            return Err(Web3Error::BadInput(
                "Bad fee input to swap_uniswap - value too large for uint24".to_string(),
            ));
        }

        let sqrt_price_limit_x96 = sqrt_price_limit_x96_uint160.clone().unwrap_or_default();
        if bad_sqrt_price_limit(&sqrt_price_limit_x96) {
            return Err(Web3Error::BadInput(
                "Bad sqrt_price_limit_x96 input to swap_uniswap - value too large for uint160"
                    .to_string(),
            ));
        }

        let eth_address = eth_private_key.to_address();
        let router = uniswap_router.unwrap_or(*UNISWAP_ROUTER_ADDRESS);
        let deadline = match deadline {
            // Default to latest block + 10 minutes
            None => self.eth_get_latest_block().await.unwrap().timestamp + (10u64 * 60u64).into(),
            Some(val) => val,
        };

        let amount_out_min: Result<Uint256, Web3Error> = if let Some(amt) = amount_out_min {
            Ok(amt)
        } else {
            self.get_sensible_amount_out_from_sqrt_price(
                sqrt_price_limit_x96_uint160,
                amount.clone(),
                token_in,
                token_out,
                fee_uint24.clone(),
                eth_address,
            )
            .await
        };
        let amount_out_min = amount_out_min?;

        //struct ExactInputSingleParams { // The uniswap exactInputSingle argument
        //    address tokenIn;
        //    address tokenOut;
        //    uint24 fee;
        //    address recipient;
        //    uint256 deadline;
        //    uint256 amountIn;
        //    uint256 amountOutMinimum;
        //    uint160 sqrtPriceLimitX96;
        //}
        let tokens: Vec<Token> = vec![
            token_in.into(),
            token_out.into(),
            fee_uint24.into(),
            eth_address.into(),
            deadline.into(),
            amount.into(),
            amount_out_min.into(),
            sqrt_price_limit_x96.into(),
        ];
        let tokens = [Token::Struct(tokens)];
        let payload = encode_call(
            "exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))",
            &tokens,
        )
        .unwrap();

        // default gas limit multiplier
        let mut options = options.unwrap_or_default();
        let glm = DEFAULT_GAS_LIMIT_MULT;
        let set_glm = options_contains_glm(&options);

        if !set_glm {
            options.push(SendTxOption::GasLimitMultiplier(glm));
        }

        let approved = self
            .check_erc20_approved(token_in, eth_address, router)
            .await?;
        if !approved {
            debug!("token_in being approved");
            // the nonce we will be using, if there's no timeout we must hack the nonce
            // of the following swap to queue properly
            let nonce = self.eth_get_transaction_count(eth_address).await?;
            let _token_in_approval = self
                .approve_erc20_transfers(
                    token_in,
                    eth_private_key,
                    router,
                    wait_timeout,
                    options.clone(),
                )
                .await?;
            if wait_timeout.is_none() {
                options.push(SendTxOption::Nonce(nonce + 1u8.into()));
            }
        }

        debug!("payload is  {:?}", payload);
        let txid = self
            .send_transaction(
                router,
                payload,
                0u32.into(),
                eth_address,
                eth_private_key,
                options,
            )
            .await?;
        debug!(
            "txid for uniswap swap is {}",
            display_uint256_as_address(txid.clone())
        );
        if let Some(timeout) = wait_timeout {
            future_timeout(
                timeout,
                self.wait_for_transaction(txid.clone(), timeout, None),
            )
            .await??;
        }

        Ok(txid)
    }

    /// Performs an exact input single pool swap via Uniswap v3, exchanging `amount` of eth directly for `token_out`
    ///
    /// IMPORTANT: normally Uniswap v3 only works with ERC20 tokens, but in the case of transfers involving wETH, they will
    /// wrap the ETH for you before the swap. Using this method you will be charged the additional gas required to wrap
    /// the input `amount` of ETH. If you will be calling this method multiple times, it is likely cheaper to wrap a lot of ETH
    /// and calling swap_uniswap() instead.
    ///
    /// # Arguments
    /// * `eth_private_key` - The private key of the holder of `token_in` who will receive `token_out`
    /// * `token_out` - The address of the ERC20 token to receive
    /// * `fee_uint24` - Optional fee level of the `token_in`<->`token_out` pool to query - limited to uint24 in size.
    ///    Defaults to the medium pool fee of 0.3%
    ///    The suggested pools are 0.3% (3000), 0.05% (500), 1% (10000), and 0.01% (100) but more may be added permissionlessly
    /// * `amount` - The amount of `token_in` to exchange for as much `token_out` as possible
    /// * `deadline` - Optional deadline to the swap before it is cancelled, 10 minutes if None
    /// * `amount_out_min` - Optional minimum amount of `token_out` to receive or the swap is cancelled,
    ///                      if None and sqrt_price_limit_x96_64 is Some(_) then a sensible value will be computed
    /// * `sqrt_price_limit_x96_64` - Optional square root price limit, ignored if None or 0. See methods below
    ///                               for how to work with this value
    /// * `uniswap_router` - Optional address of the Uniswap v3 SwapRouter to contact
    /// * `options` - Optional arguments for the Transaction, see send_transaction()
    /// * `wait_timeout` - Set to Some(TIMEOUT) if you wish to wait for this tx to enter the chain before returning
    ///
    /// # Examples
    /// ```
    /// use std::time::Duration;
    /// use clarity::PrivateKey;
    /// use web30::amm::*;
    /// use web30::client::Web3;
    /// let web3 = Web3::new("http://localhost:8545", Duration::from_secs(5));
    /// let result = web3.swap_uniswap_eth_in(
    ///     "0x1111111111111111111111111111111111111111111111111111111111111111".parse().unwrap(),
    ///     *DAI_CONTRACT_ADDRESS,
    ///     Some(500u16.into()),
    ///     1000000000000000000u128.into(), // 1 ETH
    ///     Some(60u8.into()), // Wait 1 minute
    ///     Some(2020000000000000000000u128.into()), // Expect >= 2020 DAI
    ///     Some(uniswap_sqrt_price_from_amounts(1u8.into(), 2000u16.into())), // Sample 1 Eth ->  2k Dai swap rate
    ///     Some(*UNISWAP_ROUTER_ADDRESS),
    ///     None,
    ///     None,
    /// );
    /// ```
    #[allow(clippy::too_many_arguments)]
    pub async fn swap_uniswap_eth_in(
        &self,
        eth_private_key: PrivateKey,     // the address swapping tokens
        token_out: Address,              // the desired token
        fee_uint24: Option<Uint256>,     // actually a uint24 on the callee side
        amount: Uint256,                 // the amount of tokens offered up
        deadline: Option<Uint256>,       // a deadline by which the swap must happen
        amount_out_min: Option<Uint256>, // the minimum output tokens to receive in a swap
        sqrt_price_limit_x96_uint160: Option<Uint256>, // actually a uint160 on the callee side
        uniswap_router: Option<Address>, // the default router will be used if none is provided
        options: Option<Vec<SendTxOption>>, // options for send_transaction
        wait_timeout: Option<Duration>,
    ) -> Result<Uint256, Web3Error> {
        let token_in = *WETH_CONTRACT_ADDRESS; // Uniswap requires WETH to be one of the swap tokens for ETH swaps
        let fee_uint24 = fee_uint24.unwrap_or_else(|| 3000u16.into());
        if bad_fee(&fee_uint24) {
            return Err(Web3Error::BadInput(
                "Bad fee input to swap_uniswap_eth_in - value too large for uint24".to_string(),
            ));
        }

        let sqrt_price_limit_x96 = sqrt_price_limit_x96_uint160.clone().unwrap_or_default();
        if bad_sqrt_price_limit(&sqrt_price_limit_x96) {
            return Err(Web3Error::BadInput(
                "Bad sqrt_price_limit_x96 input to swap_uniswap_eth_in - value too large for uint160"
                    .to_string(),
            ));
        }

        let eth_address = eth_private_key.to_address();
        let router = uniswap_router.unwrap_or(*UNISWAP_ROUTER_ADDRESS);
        let deadline = match deadline {
            // Default to latest block + 10 minutes
            None => self.eth_get_latest_block().await.unwrap().timestamp + (10u64 * 60u64).into(),
            Some(val) => val,
        };

        let amount_out_min: Result<Uint256, Web3Error> = if let Some(amt) = amount_out_min {
            Ok(amt)
        } else {
            self.get_sensible_amount_out_from_sqrt_price(
                sqrt_price_limit_x96_uint160,
                amount.clone(),
                *WETH_CONTRACT_ADDRESS,
                token_out,
                fee_uint24.clone(),
                eth_address,
            )
            .await
        };
        let amount_out_min = amount_out_min?;

        //struct ExactInputSingleParams { // The uniswap exactInputSingle argument
        //    address tokenIn;
        //    address tokenOut;
        //    uint24 fee;
        //    address recipient;
        //    uint256 deadline;
        //    uint256 amountIn;
        //    uint256 amountOutMinimum;
        //    uint160 sqrtPriceLimitX96;
        //}
        let tokens: Vec<Token> = vec![
            token_in.into(),
            token_out.into(),
            fee_uint24.into(),
            eth_address.into(),
            deadline.into(),
            amount.clone().into(),
            amount_out_min.into(),
            sqrt_price_limit_x96.into(),
        ];
        let tokens = [Token::Struct(tokens)];
        let payload = encode_call(
            "exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))",
            &tokens,
        )
        .unwrap();

        // default gas limit multiplier
        let mut options = options.unwrap_or_default();
        let glm = DEFAULT_GAS_LIMIT_MULT;
        let set_glm = options_contains_glm(&options);

        if !set_glm {
            options.push(SendTxOption::GasLimitMultiplier(glm));
        }

        debug!("payload is  {:?}", payload);
        let txid = self
            .send_transaction(
                router,
                payload,
                amount.clone(),
                eth_address,
                eth_private_key,
                options,
            )
            .await?;
        debug!(
            "txid for uniswap swap is {}",
            display_uint256_as_address(txid.clone())
        );
        if let Some(timeout) = wait_timeout {
            future_timeout(
                timeout,
                self.wait_for_transaction(txid.clone(), timeout, None),
            )
            .await??;
        }
        Ok(txid)
    }

    /// Requests the contract address for the Uniswap v3 pool determined by token_a, token_b, and fee_uint24 from the
    /// default or given Uniswap Factory contract
    pub async fn get_uniswap_pool_address(
        &self,
        caller_address: Address, // an unimportant ethereum address with any amount of ether
        token_a: Address,        // one of the tokens in the pool
        token_b: Address,        // the other token in the pool
        fee_uint24: Option<Uint256>, // The 0.3% fee pool will be used if not specified
        uniswap_factory: Option<Address>, // The default factory will be used if none is provided
    ) -> Result<Address, Web3Error> {
        let factory = uniswap_factory.unwrap_or(*UNISWAP_FACTORY_ADDRESS);
        let fee_uint24 = fee_uint24.unwrap_or_else(|| 3000u16.into());
        let tokens: Vec<Token> = vec![token_a.into(), token_b.into(), Token::Uint(fee_uint24)];
        let payload = encode_call("getPool(address,address,uint24)", &tokens)?;

        let pool_result = self
            .simulate_transaction(factory, 0u8.into(), payload, caller_address, None)
            .await;
        debug!("pool result is {:?}", pool_result);
        let pool_result = pool_result.unwrap();
        let zero_result = vec![0; 32];
        if pool_result == zero_result {
            return Err(Web3Error::BadResponse("No such Uniswap pool".to_string()));
        }
        let result_len = pool_result.len();
        let pool_bytes: &[u8] = &pool_result[result_len - 20..result_len];

        Ok(Address::from_slice(pool_bytes).expect("Received invalid pool address from Uniswap"))
    }

    /// Identifies token0 and token1 in a Uniswap v3 pool, which all stored data is based off of
    pub async fn get_uniswap_pool_tokens(
        &self,
        caller_address: Address, // an unimportant ethereum address with any amount of ether
        pool_addr: Address,      // the ethereum address of the Uniswap v3 pool
    ) -> Result<(Address, Address), Web3Error> {
        let token0 = self
            .get_uniswap_pool_token(caller_address, pool_addr, true)
            .await?;
        let token1 = self
            .get_uniswap_pool_token(caller_address, pool_addr, false)
            .await?;
        Ok((token0, token1))
    }

    /// Returns either token0 or token1 from a Uniswap v3 pool, depending on input
    pub async fn get_uniswap_pool_token(
        &self,
        caller_address: Address, // an unimportant ethereum address with any amount of ether
        pool_addr: Address,      // the ethereum address of the Uniswap v3 pool
        get_token_0: bool,       // The token to get, true for token0 and false for token1
    ) -> Result<Address, Web3Error> {
        let token_name = if get_token_0 { "token0" } else { "token1" };
        let payload = encode_call(&format!("{}()", token_name), &[]).unwrap();
        let token_result = self
            .simulate_transaction(pool_addr, 0u8.into(), payload, caller_address, None)
            .await?;
        debug!("token_result: {:?}", token_result);
        let result_len = token_result.len();
        let token_bytes: &[u8] = &token_result[result_len - 20..result_len];

        let token =
            Address::from_slice(token_bytes).expect("Received invalid pool address from Uniswap");
        Ok(token)
    }

    /// Fetches the "slot0" data from a Uniswap pool, which contains the following binary encoded data:
    ///     uint160 sqrtPriceX96,
    ///     int24 tick,
    ///     uint16 observationIndex,
    ///     uint16 observationCardinality,
    ///     uint16 observationCardinalityNext,
    ///     uint8 feeProtocol,
    ///     bool unlocked
    pub async fn get_uniswap_pool_slot0(
        &self,
        pool_addr: Address,      // the ethereum address of the Uniswap v3 pool
        caller_address: Address, // an unimportant ethereum address with any amount of ether
    ) -> Result<Vec<u8>, Web3Error> {
        let payload = encode_call("slot0()", &[]).unwrap();
        let slot0_result = self
            .simulate_transaction(pool_addr, 0u8.into(), payload, caller_address, None)
            .await?;
        debug!("slot0_result: {:?}", slot0_result);

        Ok(slot0_result)
    }

    /// Fetches the current sqrtPriceX96 value from the given pool
    /// sqrtPriceX96 is returned as the first value from a call to pool.slot0()
    ///
    /// Note that this value will differ slightly from the swap price due to the pool fee
    pub async fn get_uniswap_sqrt_price(
        &self,
        caller_address: Address, // an unimportant ethereum address with any amount of ether
        pool_address: Address,   // The address of the Uniswap pool contract
    ) -> Result<Uint256, Web3Error> {
        let slot0_result = self
            .get_uniswap_pool_slot0(pool_address, caller_address)
            .await?;
        debug!("slot0_result: {:?}", slot0_result);

        // we only want the first value: sqrtPriceX96, a uint160 which occupies 20 bytes but is put at the right of a 32 byte buffer
        let sqrt_price = Uint256::from_bytes_be(&slot0_result[32 - 20..32]);

        Ok(sqrt_price)
    }

    /// Returns a sensible swap amount_out for any input sqrt_price_limit, defined as the minimum swap
    /// the sqrt_price_limit would allow in an on-chain swap (sqrt_price_limit * amount)
    ///
    /// Handles the directional nature of swaps by querying the Uniswap v3 pool for its token order
    /// Returns an error if the pool given by token_in, token_out, and fee does not exist
    pub async fn get_sensible_amount_out_from_sqrt_price(
        &self,
        sqrt_price_limit: Option<Uint256>, // the sqrt price limit to be used for an on-chain swap
        amount: Uint256, // the amount of token_in to swap for an unknown amount of token_out
        token_in: Address, // the held token
        token_out: Address, // the desired token
        fee: Uint256, // the fee value of the Uniswap pool, in hundredths of basis points (e.g. 0.05% -> 500)
        caller_address: Address, // an unimportant ethereum address with any amount of ether
    ) -> Result<Uint256, Web3Error> {
        // Compute a sensible default from sqrt price limit
        if sqrt_price_limit.is_some() {
            let sqrt_price_limit = sqrt_price_limit.unwrap();
            if sqrt_price_limit == 0u8.into() {
                return Ok(0u8.into());
            }
            let decoded_price = decode_uniswap_sqrt_price(sqrt_price_limit);
            // Get the pool's ethereum address
            let addr = self
                .get_uniswap_pool_address(caller_address, token_in, token_out, Some(fee), None)
                .await?;
            // Get the order of tokens in the pool
            let (_, token1) = self.get_uniswap_pool_tokens(caller_address, addr).await?;
            let zero_for_one = token1 == token_out;
            // Uniswap sqrt price is stored as the token1 price, we flip to get the token0 price if swapping 1 -> 0
            let sensible_spot_price = if zero_for_one {
                decoded_price
            } else {
                decoded_price.inv()
            };
            let amt = amount.to_string().parse::<f64>().unwrap();
            let sensible_amount_out = sensible_spot_price * amt;
            let sensible_amount_out = sensible_amount_out.to_string().parse::<Uint256>().unwrap();
            return Ok(sensible_amount_out);
        }

        Ok(Uint256::from(0u8))
    }
}

/// Helper function that tells us wheter the options parameter has a GasLimitMultiplier set or not
fn options_contains_glm(options: &[SendTxOption]) -> bool {
    for option in options {
        match option {
            SendTxOption::GasLimitMultiplier(_) => return true,
            _ => continue,
        }
    }

    false
}

// Checks that the input fee value is within the limits of uint24
fn bad_fee(fee: &Uint256) -> bool {
    *fee > *TT24M1
}

// Checks that the input sqrt_price_limit value is within the limits of uint160
fn bad_sqrt_price_limit(sqrt_price_limit: &Uint256) -> bool {
    *sqrt_price_limit > *TT160M1
}

/// Computes the sqrt price of a pool given token_1's liquidity and token_0's liquidity
/// When used as the sqrt price limit, this calculates the maximum price that a swap
/// is allowed to push the pool to by changing the underlying liquidity without having the tx revert
/// Attempts to encode the result as a Q64.96  by copying the
/// javascript implementation (see https://en.wikipedia.org/wiki/Q_(number_format),
/// a 160 bit number v represented in Q64.96 would be equal to (v/2^96))
///
/// To convert a spot price to sqrt price, use the spot price as amount_1 and 1u8.into() as amount_0
/// or use uniswap_sqrt_price_from_price() instead
pub fn uniswap_sqrt_price_from_amounts(amount_1: Uint256, amount_0: Uint256) -> Uint256 {
    // Uniswap's javascript implementation with arguments amount1 and amount0
    //   const numerator = JSBI.leftShift(JSBI.BigInt(amount1), JSBI.BigInt(192))
    //   const denominator = JSBI.BigInt(amount0)
    //   const ratioX192 = JSBI.divide(numerator, denominator)
    //   return sqrt(ratioX192)

    // Uniswap pools contain two assets: token0 and token1
    // The price of a token is stored as a Q64.96 like so:
    //     sqrtPriceX96 = sqrt(token1Liquidity / token0Liquidity) * 2^96
    // Given a sqrtPriceX96, we can calculate price = sqrtPriceX96 ** 2 / 2 ** 192
    // If there are 10 of token1 and just 1 of token0 in a pool, the spot price should be:
    //     sqrtPriceX96 = sqrt(10 / 1) * 2^96
    // This function calculates:
    //     sqrtPriceX96 = sqrt((10 * 2^192) / 1) = sqrt(10 / 1) * sqrt(2^192) = sqrt(10 / 1) * 2^96

    let numerator: BigUint = amount_1.0 << 192; // amount1 * 2^192
    let denominator: BigUint = amount_0.0;
    let ratio_x192 = numerator / denominator;
    Uint256(BigUint::sqrt(&ratio_x192))
}

/// Encodes a given spot price as a Q64.96 sqrt price which Uniswap expects, used in limiting slippage
/// See uniswap_sqrt_price_from_amounts for the general case
pub fn uniswap_sqrt_price_from_price(spot_price: f64) -> Uint256 {
    // Because the value is a Q64.96, must scale by 2^96 (the denominator precision)
    // but because it is a square root, we scale by (2^96)^2 = 2^192, then compute the sqrt
    let sqrt_price = (spot_price * 2f64.powi(192)).sqrt();

    sqrt_price.floor().to_string().parse::<Uint256>().unwrap() // convert to Uint256
}

/// Decodes the Q64.96-encoded sqrt price from Uniswap into an intuitive price
pub fn decode_uniswap_sqrt_price(sqrt_price: Uint256) -> f64 {
    // Q64.96 values are fixed point numbers with 96 bits of fractional precision, so we divide by 2^96
    // However the uniswap value is also a square root, so we square the result as well
    let tt96 = 2f64.powi(96);
    let sqrt_price = sqrt_price.to_string().parse::<f64>().unwrap();
    (sqrt_price / tt96).powi(2)
}

/// Scales the input sqrt_price by scale factor to enable limited slippage in Uniswap swaps
/// It is necessary to first identify the direction of the swap as Uniswap depends on that for slippage calculation,
/// use get_uniswap_tokens() to receive an ordered tuple (token0: Address, token1: Address)
///
/// For a swap with token0 in and token1 out, zero_for_one must be true. Otherwise it should be false.
pub fn scale_uniswap_sqrt_price(
    sqrt_price: Uint256,   // The initial sqrt price to work with, a Q64.96
    scale_percentage: f64, // The fraction to scale by, e.g. 0.005f64 to allow 0.5% slippage
    zero_for_one: bool, // The direction of the swap true => token0 -> token1; false => token1 -> token0
) -> Uint256 {
    let spot_price = decode_uniswap_sqrt_price(sqrt_price);

    // Scale sqrt(token1 / token0) based on the direction of the swap.
    // If we are going token0 -> token1 then the new sqrtPrice should be less than our limit
    //   token1 shrinks and token0 grows so the fraction (token1 / token0) decreases
    // If we are going token1 -> token0 then the new sqrtPrice should be more than our limit
    //   token1 grows token0 shrinks so the fraction (token1 / token0) increases
    let scale_factor = if zero_for_one {
        1f64 - scale_percentage
    } else {
        1f64 + scale_percentage
    };
    let scaled_price = spot_price * scale_factor;

    uniswap_sqrt_price_from_price(scaled_price) // convert back to sqrt_price
}

/// This test acquires the sqrt price from the Uniswap v3 DAI / WETH 0.05% pool, then simulates 4 swaps with varying
/// sqrt price limits, amounts being swapped, and asserts that our sqrt price limit methods work as expected
///
/// This test is ignored because it suffers from EIP 1559 failures intermittently, where we try to specify a good
/// gas price but due to latency the transaction is rejected with GasPriceLowerThanBaseFee
#[test]
#[ignore]
fn uniswap_sqrt_price_test() {
    use actix::System;
    use futures::join;
    use std::time::Duration;
    // use env_logger::{Builder, Env};
    // Builder::from_env(Env::default().default_filter_or("error")).init();
    let runner = System::new();
    let web3 = Web3::new("https://cloudflare-eth.com/", Duration::from_secs(15));
    let caller_address =
        Address::parse_and_validate("0x5A0b54D5dc17e0AadC383d2db43B0a0D3E029c4c").unwrap();
    let one_eth = Uint256::from(1_000_000_000_000_000_000u64); // 10^18 1 eth
    let fee_0dot05_pct = Uint256::from(500u16); // 0.05%, determines the uniswap pool to use
    let no_price_limit: Uint256 = 0u8.into();

    runner.block_on(async move {
        let token_a = *WETH_CONTRACT_ADDRESS;
        let token_b = *DAI_CONTRACT_ADDRESS;

        let pool_addr = web3
            .get_uniswap_pool_address(
                caller_address,
                token_a,
                token_b,
                Some(fee_0dot05_pct.clone()),
                None,
            )
            .await
            .unwrap();
        let tokens = web3
            .get_uniswap_pool_tokens(caller_address, pool_addr)
            .await;
        info!("tokens result: {:?}", tokens);
        let tokens = tokens.unwrap();

        let price = web3
            .get_uniswap_price(
                caller_address,
                token_a,
                token_b,
                Some(fee_0dot05_pct.clone()),
                one_eth.clone(),
                Some(no_price_limit),
                None,
            )
            .await;
        let weth2dai = price.unwrap();
        info!("weth->dai current price is {}", weth2dai);

        let pool = web3
            .get_uniswap_pool_address(
                caller_address,
                token_a,
                token_b,
                Some(fee_0dot05_pct.clone()),
                None,
            )
            .await
            .unwrap();

        let sqrt_price = web3.get_uniswap_sqrt_price(caller_address, pool).await;
        let sqrt_price = sqrt_price.unwrap();

        let spot_price_token0 = decode_uniswap_sqrt_price(sqrt_price.clone());
        let spot_price_token1 = spot_price_token0.inv();
        info!(
            "Calculated token0 ({}) worth in token1 ({}): {}",
            tokens.0, tokens.1, spot_price_token0,
        );
        info!(
            "Calculated token1 ({}) worth in token0 ({}): {}",
            tokens.1, tokens.0, spot_price_token1,
        );

        let little_pad_factor = 0.001f64;
        let little_padded_sqrt_price_0_to_1 =
            scale_uniswap_sqrt_price(sqrt_price.clone(), little_pad_factor, true);
        let little_padded_sqrt_price_1_to_0 =
            scale_uniswap_sqrt_price(sqrt_price.clone(), little_pad_factor, false);

        let pad_factor = 0.05f64; // 5% tolerance
        let padded_sqrt_price_0_to_1 =
            scale_uniswap_sqrt_price(sqrt_price.clone(), pad_factor, true);
        info!(
            "Calculated padded 0->1 sqrt price limit: {}, original {}",
            decode_uniswap_sqrt_price(padded_sqrt_price_0_to_1.clone()),
            spot_price_token0.clone(),
        );
        let padded_sqrt_price_1_to_0 =
            scale_uniswap_sqrt_price(sqrt_price.clone(), pad_factor, false);
        info!(
            "Calculated padded 1->0 sqrt price limit: {}, original {}",
            decode_uniswap_sqrt_price(padded_sqrt_price_1_to_0.clone()),
            spot_price_token0.clone(),
        );
        let little_eth = one_eth.clone(); // One Ether
        let little_dai = one_eth.clone() * 2_000u32.into(); // $2k
        let lots_of_eth = one_eth.clone() * 100u32.into(); // 100 Ether
        let lots_of_dai = one_eth.clone() * 200_000u32.into(); // $200k

        // Test two swaps with low slippage tolerance and a small amount
        let a = attempt_swap_with_limit(
            &web3,
            11,
            caller_address,
            tokens.0, // DAI
            tokens.1, // ETH
            sqrt_price.clone(),
            little_padded_sqrt_price_0_to_1.clone(), // No slippage
            little_dai.clone(),
            fee_0dot05_pct.clone(),
            false,
        );

        let b = attempt_swap_with_limit(
            &web3,
            21,
            caller_address,
            tokens.1, // ETH
            tokens.0, // DAI
            sqrt_price.clone(),
            little_padded_sqrt_price_1_to_0.clone(), // No slippage
            little_eth.clone(),
            fee_0dot05_pct.clone(),
            false,
        );

        // Test two swaps with `pad_factor` slippage tolerance and an amount which should push the price past tolerance
        // These swaps should be either reverted or result in too little token output
        let c = attempt_swap_with_limit(
            &web3,
            31,
            caller_address,
            tokens.0, // DAI
            tokens.1, // ETH
            sqrt_price.clone(),
            padded_sqrt_price_0_to_1.clone(), // With slippage
            lots_of_dai.clone(),
            fee_0dot05_pct.clone(),
            true,
        );

        let d = attempt_swap_with_limit(
            &web3,
            41,
            caller_address,
            tokens.1, // ETH
            tokens.0, // DAI
            sqrt_price.clone(),
            padded_sqrt_price_1_to_0.clone(), // With slippage
            lots_of_eth.clone(),
            fee_0dot05_pct.clone(),
            true,
        );
        join!(a, b, c, d);
    });
}

/// A test utility function which will get a swap price from the quoter and assert that the amount out is reasonable
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
async fn attempt_swap_with_limit(
    web3: &Web3,
    i: i32,                            // an identifier for logs
    caller_address: Address,           // an unimportant ethereum address with some amount of ether
    token_in: Address,                 // the held token
    token_out: Address,                // the desired token
    sqrt_price_no_slippage: Uint256,   // the current sqrt price stored in the uniswap pool
    sqrt_price_with_slippage: Uint256, // a sqrt price with a bit of slippage tolerance factored in
    amount: Uint256,                   // the amount to swap
    pool_fee: Uint256, // the fee level of the pool, given in hundredths of basis points (e.g. 0.05% -> 500)
    expect_failure: bool, // whether or not the amount swapped should violate sqrt_price_with_slippage, causing a panic
) {
    let base_spot_price = decode_uniswap_sqrt_price(sqrt_price_no_slippage);
    let slippage_spot_price = decode_uniswap_sqrt_price(sqrt_price_with_slippage.clone());
    let slippage_tolerance = slippage_spot_price - base_spot_price;
    let pretty_amount = amount.to_string().parse::<f64>().unwrap() / 10f64.powi(18);
    info!(
        "{}: Attempting swap with {} slippage - sqrt_price {}, amount {}, token_in {}, token_out {}",
        i,
        slippage_tolerance,
        decode_uniswap_sqrt_price(sqrt_price_with_slippage.clone()),
        pretty_amount,
        token_in,
        token_out,
    );
    let swap_out = web3
        .get_uniswap_price(
            caller_address,
            token_in,
            token_out,
            Some(pool_fee.clone()),
            amount.clone(),
            Some(sqrt_price_with_slippage.clone()),
            None,
        )
        .await;
    info!(
        "{}: get_uniswap_price with limit {:.8}: result {:?}",
        i, slippage_spot_price, swap_out
    );
    if swap_out.is_err() {
        if !expect_failure {
            panic!("Swap failed! {}", swap_out.unwrap_err());
        }
        return;
    }
    let swap_out = swap_out.unwrap();

    // We expect at least the worst slippage amount out of the swap
    let expected_out_0_1 = (pretty_amount * slippage_spot_price) * 10f64.powi(18);
    let expected_out_1_0 = pretty_amount * slippage_spot_price.inv() * 10f64.powi(18);
    let f_swap = swap_out.to_string().parse::<f64>().unwrap();
    if f_swap < expected_out_0_1 && f_swap < expected_out_1_0 {
        if !expect_failure {
            panic!(concat!(
                "{} Found that the tokens we got out {} are less than we would expect for a 0>1 {} ",
                    "and for a 1>0 swap {}, this should have been covered earlier!"),
                i, f_swap, expected_out_0_1, expected_out_1_0
            );
        }
        info!(
            "Received {} {} for {} {}, expected amounts were [{} or {}]",
            f_swap,
            token_out,
            amount.clone(),
            token_in,
            expected_out_0_1,
            expected_out_1_0
        );
    }
}

#[ignore]
#[test]
fn get_uniswap_price_test() {
    use actix::System;
    use env_logger::{Builder, Env};
    use std::time::Duration;
    Builder::from_env(Env::default().default_filter_or("warn")).init(); // Change to debug for logs
    let runner = System::new();
    let web3 = Web3::new("https://eth.althea.net", Duration::from_secs(5));
    let caller_address =
        Address::parse_and_validate("0x5A0b54D5dc17e0AadC383d2db43B0a0D3E029c4c").unwrap();
    let amount = Uint256::from(1_000_000_000_000_000_000u64);
    let fee = Uint256::from(500u16);
    let sqrt_price_limit_x96_uint160 = Uint256::from(0u16);

    runner.block_on(async move {
        let price = web3
            .get_uniswap_price(
                caller_address,
                *WETH_CONTRACT_ADDRESS,
                *DAI_CONTRACT_ADDRESS,
                Some(fee.clone()),
                amount.clone(),
                Some(sqrt_price_limit_x96_uint160.clone()),
                None,
            )
            .await;
        let weth2dai = price.unwrap();
        debug!("weth->dai price is {}", weth2dai);
        assert!(weth2dai > 0u32.into());
        let price = web3
            .get_uniswap_price(
                caller_address,
                *DAI_CONTRACT_ADDRESS,
                *WETH_CONTRACT_ADDRESS,
                Some(fee.clone()),
                weth2dai,
                Some(sqrt_price_limit_x96_uint160),
                None,
            )
            .await;
        let dai2weth = price.unwrap();
        debug!("dai->weth price is {}", &dai2weth);
        let amount_float: f64 = (amount.to_string()).parse().unwrap();
        let dai2weth_float: f64 = (dai2weth.to_string()).parse().unwrap();
        // If we were to swap, we should get within 5% back what we originally put in to account for slippage and fees
        assert!((0.95 * amount_float) < dai2weth_float && dai2weth_float < (1.05 * amount_float));
    });
}

#[test]
// Avoid accidentally spending funds or failing when not running hardhat
#[ignore]
// Note: If you specify a live eth node in Web3::new() and a real private key below, real funds will be used.
// Run this test with the local hardhat environment running
// Swaps WETH for DAI then back again
fn swap_hardhat_test() {
    // this key is the private key for the public key defined in tests/assets/ETHGenesis.json
    // where the full node / miner sends its rewards. Therefore it's always going
    // to have a lot of ETH to pay for things like contract deployments
    let miner_private_key: PrivateKey =
        "0xb1bab011e03a9862664706fc3bbaa1b16651528e5f0e7fbfcbfdd8be302a13e7"
            .parse()
            .unwrap();
    let miner_address: Address = miner_private_key.to_address();

    use crate::client::Web3;
    use actix::System;
    use env_logger::{Builder, Env};
    use std::time::Duration;
    Builder::from_env(Env::default().default_filter_or("warn")).init(); // Change to debug for logs
    let runner = System::new();

    let web3 = Web3::new("http://localhost:8545", Duration::from_secs(300));
    let amount = Uint256::from(1000000000000000000u64); // 1 weth
    let amount_out_min: Uint256 = 0u8.into();
    let fee = Uint256::from(500u16);

    let sqrt_price_limit_x96_uint160: Uint256 = 0u8.into();
    runner.block_on(async move {
        let block = web3.eth_get_latest_block().await.unwrap();
        let deadline = block.timestamp + (10u32 * 60u32 * 100000u32).into();

        let success = web3
            .wrap_eth(amount.clone(), miner_private_key, None, None)
            .await;
        if let Ok(b) = success {
            info!("Wrapped eth: {}", b);
        } else {
            panic!("Failed to wrap eth before testing uniswap");
        }
        let initial_weth = web3
            .get_erc20_balance(*WETH_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();
        let initial_dai = web3
            .get_erc20_balance(*DAI_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();

        info!(
            "Initial WETH: {}, Initial DAI: {}",
            initial_weth, initial_dai
        );

        let result = web3
            .swap_uniswap(
                miner_private_key,
                *WETH_CONTRACT_ADDRESS,
                *DAI_CONTRACT_ADDRESS,
                Some(fee.clone()),
                amount.clone(),
                Some(deadline.clone()),
                Some(amount_out_min.clone()),
                Some(sqrt_price_limit_x96_uint160.clone()),
                None,
                None,
                None,
            )
            .await;
        if result.is_err() {
            panic!("Error performing first swap: {:?}", result.err());
        }
        let executing_weth = web3
            .get_erc20_balance(*WETH_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();
        let executing_dai = web3
            .get_erc20_balance(*DAI_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();
        info!(
            "Executing WETH: {}, Executing DAI: {}",
            executing_weth, executing_dai
        );

        let dai_gained = executing_dai.clone() - initial_dai.clone();
        assert!(dai_gained > 0u8.into());
        let result = web3
            .swap_uniswap(
                miner_private_key,
                *DAI_CONTRACT_ADDRESS,
                *WETH_CONTRACT_ADDRESS,
                Some(fee.clone()),
                dai_gained.clone(),
                Some(deadline.clone()),
                Some(amount_out_min.clone()),
                Some(sqrt_price_limit_x96_uint160.clone()),
                None,
                None,
                None,
            )
            .await;
        if result.is_err() {
            panic!("Error performing second swap: {:?}", result.err());
        }
        let final_weth = web3
            .get_erc20_balance(*WETH_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();
        let final_dai = web3
            .get_erc20_balance(*DAI_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();
        info!("Final WETH: {}, Final DAI: {}", final_weth, final_dai);
        let final_dai_delta = final_dai - initial_dai;
        assert!(final_dai_delta == 0u8.into()); // We should have gained little to no dai

        let weth_gained: f64 = (final_weth - executing_weth).to_string().parse().unwrap();
        let original_amount: f64 = (amount).to_string().parse().unwrap();
        // we should not have lost or gained much
        assert!(0.95 * original_amount < weth_gained && weth_gained < 1.05 * original_amount);
    });
}

#[test]
// Avoid accidentally spending funds or failing when not running hardhat
#[ignore]
// Note: If you specify a live eth node in Web3::new() and a real private key below, real funds will be used.
// Run this test with the local hardhat environment running
// Swaps WETH for DAI then back again
fn swap_hardhat_eth_in_test() {
    // this key is the private key for the public key defined in tests/assets/ETHGenesis.json
    // where the full node / miner sends its rewards. Therefore it's always going
    // to have a lot of ETH to pay for things like contract deployments
    let miner_private_key: PrivateKey =
        "0xb1bab011e03a9862664706fc3bbaa1b16651528e5f0e7fbfcbfdd8be302a13e7"
            .parse()
            .unwrap();
    let miner_address: Address = miner_private_key.to_address();

    use crate::client::Web3;
    use actix::System;
    use env_logger::{Builder, Env};
    use std::time::Duration;
    Builder::from_env(Env::default().default_filter_or("warn")).init(); // Change to warn for logs
    let runner = System::new();

    let web3 = Web3::new("http://localhost:8545", Duration::from_secs(300));
    let amount = Uint256::from(1000000000000000000u64); // 1 weth
    let amount_out_min: Uint256 = 0u8.into();
    let fee = Uint256::from(500u16);

    let sqrt_price_limit_x96_uint160: Uint256 = 0u8.into();
    runner.block_on(async move {
        let block = web3.eth_get_latest_block().await.unwrap();
        let deadline = block.timestamp + (10u32 * 60u32 * 100000u32).into();

        let initial_eth = web3.eth_get_balance(miner_address).await.unwrap();
        let initial_weth = web3
            .get_erc20_balance(*WETH_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();
        let initial_dai = web3
            .get_erc20_balance(*DAI_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();

        info!(
            "Initial ETH: {}, Initial WETH: {}, Initial DAI: {}",
            initial_eth, initial_weth, initial_dai
        );
        let result = web3
            .swap_uniswap_eth_in(
                miner_private_key,
                *DAI_CONTRACT_ADDRESS,
                Some(fee.clone()),
                amount.clone(),
                Some(deadline.clone()),
                Some(amount_out_min.clone()),
                Some(sqrt_price_limit_x96_uint160.clone()),
                None,
                None,
                None,
            )
            .await;
        if result.is_err() {
            panic!("Error performing first swap: {:?}", result.err());
        }
        let final_eth = web3.eth_get_balance(miner_address).await.unwrap();
        let final_weth = web3
            .get_erc20_balance(*WETH_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();
        let final_dai = web3
            .get_erc20_balance(*DAI_CONTRACT_ADDRESS, miner_address)
            .await
            .unwrap();
        info!(
            "Final ETH: {}, Final WETH: {}, Final DAI: {}",
            final_eth, final_weth, final_dai
        );

        let dai_gained = final_dai - initial_dai;
        // At the point the chain is frozen for the relay market test,
        // we expect to receive expect to receive about 2,300 dai
        let two_k_dai = 2000 * 1_000_000_000_000_000_000u128;
        let one_eth = 1_000_000_000_000_000_000u128;
        assert!(
            dai_gained > two_k_dai.into(),
            "dai_gained = {} <= 2000 * 10^18",
            dai_gained
        );
        let eth_lost = initial_eth - final_eth;
        assert!(
            eth_lost > one_eth.into(),
            "eth_lost = {} <= 1 * 10^18",
            eth_lost
        );

        assert!(
            final_weth == initial_weth,
            "Did not expect to modify wETH balance. Started with {} ended with {}",
            initial_weth,
            final_weth
        );

        info!(
            "Effectively swapped {} eth for {} dai",
            eth_lost, dai_gained
        );
    });
}
