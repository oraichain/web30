use serde_json::Value;

use crate::types::ConciseBlock;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JsonRpcError<E> {
    pub code: i64,
    pub message: String,
    pub data: Option<E>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum ResponseData<R, E> {
    Error { error: JsonRpcError<E> },
    Success { result: R },
}

impl<R, E> ResponseData<R, E> {
    /// Consume response and return value
    pub fn into_result(self) -> Result<R, JsonRpcError<E>> {
        match self {
            ResponseData::Success { result } => Ok(result),
            ResponseData::Error { error } => Err(error),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Response<R, E = Value> {
    pub id: Value,
    pub jsonrpc: String,
    #[serde(flatten)]
    pub data: ResponseData<R, E>,
}

#[test]
fn test_response() {
    let response: Response<u64> =
        serde_json::from_str(r#"{"jsonrpc": "2.0", "result": 19, "id": 1}"#).unwrap();
    assert_eq!(response.id.as_u64().unwrap(), 1);
    assert_eq!(response.data.into_result().unwrap(), 19);
}

#[test]
fn test_response_bytes() {
    println!(
        "res json: {:?}",
        serde_json::from_slice::<Response<ConciseBlock>>(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"baseFeePerGas\":\"0x0\",\"difficulty\":\"0x0\",\"extraData\":\"0x\",\"gasLimit\":\"0x0\",\"gasUsed\":\"0x0\",\"hash\":\"0x000000000216f244f2c0b4b10c3bc59b3fd149d903cd18eebcfcf4bee0ffd21e\",\"logsBloom\":\"0x00000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000\",\"miner\":\"0x4bbb38ac8f2c666f56c16e0b21ccda04d18b6b6b\",\"mixHash\":\"0x0000000000000000000000000000000000000000000000000000000000000000\",\"nonce\":\"0x0000000000000000\",\"number\":\"0x216f244\",\"parentHash\":\"0x000000000216f2434829f5424450b17b46b00929b3aaa337805de99d819ba976\",\"receiptsRoot\":\"0x0000000000000000000000000000000000000000000000000000000000000000\",\"sha3Uncles\":\"0x0000000000000000000000000000000000000000000000000000000000000000\",\"size\":\"0xb1\",\"stateRoot\":\"0x\",\"timestamp\":\"0x64136466\",\"totalDifficulty\":\"0x0\",\"transactions\":[],\"transactionsRoot\":\"0x0000000000000000000000000000000000000000000000000000000000000000\",\"uncles\":[]}}")
    );
}

#[test]
fn test_error() {
    let response: Response<Value> = serde_json::from_str(r#"{"jsonrpc": "2.0", "error": {"code": -32601, "message": "Method not found"}, "id": "1"}"#).unwrap();
    assert_eq!(response.id.as_str().unwrap(), "1");
    let err = response.data.into_result().unwrap_err();
    assert_eq!(err.code, -32601);
    assert_eq!(err.message, "Method not found");
}
