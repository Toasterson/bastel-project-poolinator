use miette::{IntoDiagnostic, Result};
use serde::{Deserialize, Serialize};
use std::io::stdin;

#[derive(Serialize, Deserialize)]
struct Sample {
    input: serde_json::Value,
    output: String,
}

fn main() -> Result<()> {
    let val: serde_json::Value = serde_json::from_reader(stdin()).into_diagnostic()?;
    let r = Sample {
        input: val,
        output: String::from("testing"),
    };
    let s = serde_json::to_string(&r).into_diagnostic()?;
    println!("{s}");
    Ok(())
}
