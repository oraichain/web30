use crate::amm::WETH_CONTRACT_ADDRESS;
use crate::{client::Web3, jsonrpc::error::Web3Error};
use clarity::abi::AbiToken as Token;
use clarity::Address;
use clarity::{PrivateKey, Uint256};
use std::time::Duration;
use tokio::time::timeout as future_timeout;

// Performs wrapping and unwrapping of eth, along with balance checking
impl Web3 {
    pub async fn wrap_eth(
        &self,
        amount: Uint256,
        secret: PrivateKey,
        weth_address: Option<Address>,
        wait_timeout: Option<Duration>,
    ) -> Result<Uint256, Web3Error> {
        let own_address = secret.to_address();
        let sig = "deposit()";
        let tokens = [];
        let weth_address = weth_address.unwrap_or(*WETH_CONTRACT_ADDRESS);
        let txid = self
            .send_transaction(
                weth_address,
                sig,
                &tokens,
                amount,
                own_address,
                secret,
                vec![],
            )
            .await?;

        if let Some(timeout) = wait_timeout {
            future_timeout(timeout, self.wait_for_transaction(txid, timeout, None)).await??;
        }
        Ok(txid)
    }

    pub async fn unwrap_eth(
        &self,
        amount: Uint256,
        secret: PrivateKey,
        weth_address: Option<Address>,
        wait_timeout: Option<Duration>,
    ) -> Result<Uint256, Web3Error> {
        let own_address = secret.to_address();
        let sig = "withdraw(uint256)";
        let tokens = [Token::Uint(amount)];
        let weth_address = weth_address.unwrap_or(*WETH_CONTRACT_ADDRESS);
        let txid = self
            .send_transaction(
                weth_address,
                sig,
                &tokens,
                0u16.into(),
                own_address,
                secret,
                vec![],
            )
            .await?;

        if let Some(timeout) = wait_timeout {
            future_timeout(timeout, self.wait_for_transaction(txid, timeout, None)).await??;
        }
        Ok(txid)
    }
}
