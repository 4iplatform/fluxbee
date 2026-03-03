use std::io::{self, Read};
use std::sync::Arc;

use fluxbee_ai_sdk::{Agent, OpenAiResponsesClient};
use tracing_subscriber::EnvFilter;

fn usage() -> String {
    "usage: ai_local_probe --input \"hello\" [--model gpt-4.1-mini] [--instructions \"...\"] [--api-key-env OPENAI_API_KEY] [--base-url https://api.openai.com/v1/responses]\n\
     alternatively omit --input to read stdin"
        .to_string()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let args = parse_args()?;
    let api_key = std::env::var(&args.api_key_env).map_err(|_| {
        format!(
            "missing required env var for OpenAI api key: {}",
            args.api_key_env
        )
    })?;

    let mut client = OpenAiResponsesClient::new(api_key);
    if let Some(base_url) = &args.base_url {
        client = client.with_base_url(base_url.clone());
    }

    let agent = Agent::new(
        "ai_local_probe",
        args.model,
        args.instructions,
        Arc::new(client),
    );
    let response = agent.run_text(args.input).await?;
    println!("{}", response.content);
    Ok(())
}

struct ProbeArgs {
    input: String,
    model: String,
    instructions: Option<String>,
    api_key_env: String,
    base_url: Option<String>,
}

fn parse_args() -> Result<ProbeArgs, Box<dyn std::error::Error + Send + Sync>> {
    let raw = std::env::args().skip(1).collect::<Vec<_>>();
    let mut input = None::<String>;
    let mut model = "gpt-4.1-mini".to_string();
    let mut instructions = None::<String>;
    let mut api_key_env = "OPENAI_API_KEY".to_string();
    let mut base_url = None::<String>;

    let mut i = 0usize;
    while i < raw.len() {
        match raw[i].as_str() {
            "--input" => {
                let Some(value) = raw.get(i + 1) else {
                    return Err("missing value for --input".to_string().into());
                };
                input = Some(value.clone());
                i += 2;
            }
            "--model" => {
                let Some(value) = raw.get(i + 1) else {
                    return Err("missing value for --model".to_string().into());
                };
                model = value.clone();
                i += 2;
            }
            "--instructions" => {
                let Some(value) = raw.get(i + 1) else {
                    return Err("missing value for --instructions".to_string().into());
                };
                instructions = Some(value.clone());
                i += 2;
            }
            "--api-key-env" => {
                let Some(value) = raw.get(i + 1) else {
                    return Err("missing value for --api-key-env".to_string().into());
                };
                api_key_env = value.clone();
                i += 2;
            }
            "--base-url" => {
                let Some(value) = raw.get(i + 1) else {
                    return Err("missing value for --base-url".to_string().into());
                };
                base_url = Some(value.clone());
                i += 2;
            }
            "--help" | "-h" => {
                println!("{}", usage());
                std::process::exit(0);
            }
            other => {
                return Err(format!("unknown argument: {other}\n{}", usage()).into());
            }
        }
    }

    let input = match input {
        Some(value) => value,
        None => read_stdin()?,
    };

    if input.trim().is_empty() {
        return Err("empty input; pass --input or provide stdin".to_string().into());
    }

    Ok(ProbeArgs {
        input,
        model,
        instructions,
        api_key_env,
        base_url,
    })
}

fn read_stdin() -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}
