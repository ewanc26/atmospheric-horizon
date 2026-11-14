use serde::Deserialize;

/// Body for an operator label emission. The server signs with its configured
/// labeler DID and stamps `cts`; callers cannot choose `src`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmitLabelInput {
    pub uri: String,
    pub val: String,
    #[serde(default)]
    pub neg: bool,
    #[serde(default)]
    pub cid: Option<String>,
    #[serde(default)]
    pub exp: Option<String>,
}
