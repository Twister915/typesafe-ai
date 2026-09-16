mod support;

use std::{env, error::Error};

use typesafe_ai::UreqClient;

fn main() -> Result<(), Box<dyn Error>> {
    let client = UreqClient::new(env::var("TYPESAFE_API_KEY")?)?;
    let response = client.evaluate(&support::request())?;
    support::print_answers(&response);
    Ok(())
}
