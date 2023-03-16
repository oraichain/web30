use clarity::{
    abi::{encode_tokens, Token},
    Address as EthAddress, PrivateKey, Uint256,
};
use heliosphere::{
    signer::{keypair::Keypair, signer::Signer},
    MethodCall, RpcClient,
};
use num_traits::ToPrimitive;

use crate::{jsonrpc::error::Web3Error, types::SendTxOption};

pub async fn send_transaction(
    client: &RpcClient,
    contract: EthAddress,
    selector: &str,
    tokens: &[Token],
    value: Uint256,
    sender_secret: PrivateKey,
    options: Vec<SendTxOption>,
) -> Result<Uint256, Web3Error> {
    // extract method name for logging

    // processing with tron
    // this is tron, we need to create a tron instance from web3
    let keypair = Keypair::from_bytes(&sender_secret.to_bytes()).expect("Wrong secret key");

    let method_call = MethodCall {
        caller: &keypair.address(),
        contract: &contract.into(),
        selector,
        parameter: &encode_tokens(tokens),
    };

    // Estimate energy usage
    let estimated = client.estimate_fee_limit(&method_call).await? as f64;
    let mut gas_limit_multiplier = 1f64;
    for option in options {
        match option {
            SendTxOption::GasLimitMultiplier(glm) => {
                gas_limit_multiplier = glm.into();
                break;
            }
            _ => continue,
        }
    }
    let fee_limit = (estimated * gas_limit_multiplier).round() as u64;

    // Send tx
    let mut tx = client
        .trigger_contract(
            &method_call,
            value.to_u64().expect("Value overflow!"),
            Some(fee_limit),
        )
        .await?;
    keypair.sign_transaction(&mut tx).unwrap();
    let tx_id = client.broadcast_transaction(&tx).await?;

    Ok(Uint256::from_be_bytes(&tx_id.0))
}
