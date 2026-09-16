mod support;

use std::{env, error::Error};

use typesafe_ai::ReqwestClient;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn Error>> {
    let client = ReqwestClient::new(env::var("TYPESAFE_API_KEY")?)?;
    let response = client.evaluate(&support::request()).await?;
    support::print_answers(&response);
    Ok(())
}
